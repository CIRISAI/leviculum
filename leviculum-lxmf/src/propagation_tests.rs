use super::*;
use alloc::vec;

fn id(start: u8) -> TransientId {
    let mut id = [0u8; 32];
    for (offset, byte) in id.iter_mut().enumerate() {
        *byte = start + offset as u8;
    }
    id
}

#[test]
fn client_codecs_round_trip_for_the_test_mailbox() {
    let request = MessageGetRequest {
        wants: Some(vec![id(32)]),
        haves: Some(vec![id(64)]),
        transfer_limit_kb: Some(TransferLimit::Integer(1000)),
    };
    let encoded = request.encode().unwrap();
    assert_eq!(MessageGetRequest::decode(&encoded).unwrap(), request);
    let list_request = MessageGetRequest::list();
    assert_eq!(
        MessageGetRequest::decode(&list_request.encode().unwrap()).unwrap(),
        list_request
    );
    let acknowledge = MessageGetRequest::acknowledge(vec![id(0)]);
    assert_eq!(
        MessageGetRequest::decode(&acknowledge.encode().unwrap()).unwrap(),
        acknowledge
    );

    let list = MessageListResponse::TransientIds(vec![id(32), id(64)]);
    assert_eq!(
        MessageListResponse::decode(&list.encode().unwrap()).unwrap(),
        list
    );

    let messages = MessageGetResponse::Messages(vec![b"one".to_vec(), b"two".to_vec()]);
    assert_eq!(
        MessageGetResponse::decode(&messages.encode().unwrap()).unwrap(),
        messages
    );

    for error in [PeerError::NoIdentity, PeerError::NoAccess] {
        let list_error = MessageListResponse::Error(error);
        assert_eq!(
            MessageListResponse::decode(&list_error.encode().unwrap()).unwrap(),
            list_error
        );
        let get_error = MessageGetResponse::Error(error);
        assert_eq!(
            MessageGetResponse::decode(&get_error.encode().unwrap()).unwrap(),
            get_error
        );
    }
}

#[test]
fn propagation_announce_round_trips_for_discovery_tests() {
    let announce = PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 1_700_000_000,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 16,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        metadata: vec![(1, vec![0xc4, 4, b'N', b'o', b'd', b'e'])],
    };
    assert_eq!(
        PropagationNodeAnnounce::decode(&announce.encode().unwrap()).unwrap(),
        announce
    );
}

#[test]
fn propagation_announce_accepts_lxmf_1_1_float_limit() {
    let bytes = hex::decode(
        "97c2ce6a693f31c3cb4090000000000000cd2800931003128101c413\
         50726f7061676174696f6e204e6f6465204e4c"
            .replace(char::is_whitespace, ""),
    )
    .unwrap();

    let announce = PropagationNodeAnnounce::decode(&bytes).unwrap();
    assert_eq!(announce.transfer_limit_kb, 1_024);
    assert_eq!(announce.sync_limit_kb, 10_240);
    assert_eq!(
        announce.metadata,
        vec![(
            1,
            [&[0xc4, 0x13], b"Propagation Node NL".as_slice(),].concat(),
        )]
    );
}

#[test]
fn propagation_announce_float_limits_follow_python_int_semantics() {
    let mut bytes = Vec::new();
    msgpack::array(&mut bytes, 7);
    msgpack::bool(&mut bytes, false);
    msgpack::uint(&mut bytes, 1_700_000_000);
    msgpack::bool(&mut bytes, true);
    msgpack::f64(&mut bytes, 0.38);
    msgpack::f64(&mut bytes, 10_240.9);
    msgpack::array(&mut bytes, 3);
    msgpack::uint(&mut bytes, 16);
    msgpack::uint(&mut bytes, 3);
    msgpack::uint(&mut bytes, 18);
    msgpack::map(&mut bytes, 0);

    let announce = PropagationNodeAnnounce::decode(&bytes).unwrap();
    assert_eq!(announce.transfer_limit_kb, 0);
    assert_eq!(announce.sync_limit_kb, 10_240);
}

#[test]
fn propagation_announce_rejects_invalid_float_limits() {
    for (value, expected) in [
        (-0.1, PropagationError::InvalidValue),
        (f64::NAN, PropagationError::InvalidValue),
        (f64::INFINITY, PropagationError::InvalidValue),
        (u64::MAX as f64, PropagationError::Overflow),
    ] {
        let mut bytes = Vec::new();
        msgpack::array(&mut bytes, 7);
        msgpack::bool(&mut bytes, false);
        msgpack::uint(&mut bytes, 1_700_000_000);
        msgpack::bool(&mut bytes, true);
        msgpack::f64(&mut bytes, value);
        msgpack::uint(&mut bytes, 10_240);
        msgpack::array(&mut bytes, 3);
        msgpack::uint(&mut bytes, 16);
        msgpack::uint(&mut bytes, 3);
        msgpack::uint(&mut bytes, 18);
        msgpack::map(&mut bytes, 0);

        assert_eq!(PropagationNodeAnnounce::decode(&bytes), Err(expected));
    }
}

#[test]
fn malformed_and_trailing_values_are_rejected() {
    assert_eq!(
        MessageGetRequest::decode(&[0x92, 0xc0, 0xc0, 0x00]),
        Err(PropagationError::TrailingData)
    );
}

#[test]
fn singleton_upload_matches_python_msgpack_golden_vector() {
    let unstamped: Vec<u8> = (0..48).collect();
    let mut stamp = [0u8; STAMP_SIZE];
    for (offset, byte) in stamp.iter_mut().enumerate() {
        *byte = 0xa0 + offset as u8;
    }
    let upload = PropagationUpload::single(1_700_000_000.25, unstamped, stamp);

    assert_eq!(
        upload.transient_id(),
        &[
            0x4d, 0xbd, 0xc2, 0xb2, 0xb6, 0x2c, 0xb0, 0x07, 0x49, 0x78, 0x5b, 0xc8, 0x42, 0x02,
            0x23, 0x6d, 0xbc, 0x37, 0x77, 0xd7, 0x46, 0x60, 0x61, 0x1b, 0x8e, 0x58, 0x81, 0x2f,
            0x0c, 0xfd, 0xe6, 0xc3,
        ]
    );
    assert_eq!(
        upload.encode(),
        vec![
            0x92, 0xcb, 0x41, 0xd9, 0x54, 0xfc, 0x40, 0x10, 0x00, 0x00, 0x91, 0xc4, 0x50, 0x00,
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a,
            0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8,
            0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
            0xb7, 0xb8, 0xb9, 0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf,
        ]
    );
}

#[test]
fn invalid_stamp_signal_is_exact_and_strict() {
    assert_eq!(
        PropagationSignal::decode(&[0x91, 0xcc, 0xf5]),
        Ok(PropagationSignal::InvalidStamp)
    );
    assert_eq!(
        PropagationSignal::decode(&[0x92, 0xcc, 0xf5, 0xcc, 0xf5]),
        Err(PropagationError::InvalidLength)
    );
    assert_eq!(
        PropagationSignal::decode(&[0x91, 0xcc, 0xf4]),
        Err(PropagationError::InvalidValue)
    );
    assert_eq!(
        PropagationSignal::decode(&[0x91, 0xcc, 0xf5, 0xc0]),
        Err(PropagationError::TrailingData)
    );
}

// leviculum#38 — the HOST direction: everything a propagation node needs to
// parse from and serialize to a client, driven strictly through the public
// API surface an external crate (CIRISEdge) sees.

/// A host decodes the client's upload envelope back to the exact typed value
/// the client built, transient ID included, and rejects malformed shapes.
#[test]
fn host_decodes_client_upload_envelope() {
    let unstamped = {
        let mut v = vec![0x11u8; DESTINATION_LENGTH]; // destination hash
        v.extend_from_slice(&[0xAB; 64]); // ciphertext-ish body
        v
    };
    let upload = PropagationUpload::single(1_700_000_000.5, unstamped, [0x5A; STAMP_SIZE]);
    let wire = upload.encode();

    let decoded = PropagationUpload::decode(&wire).expect("round-trip");
    assert_eq!(decoded, upload);
    assert_eq!(decoded.transient_id(), upload.transient_id());

    // Too short to carry hash + body + stamp → refused.
    let tiny =
        PropagationUpload::single(1.0, vec![0u8; DESTINATION_LENGTH], [0u8; STAMP_SIZE]).encode();
    assert_eq!(
        PropagationUpload::decode(&tiny),
        Err(PropagationError::InvalidLength)
    );
    assert!(PropagationUpload::decode(&[0x91]).is_err());
}

/// A host serves the full `/get` exchange with the now-public codecs: decode
/// the client's request, encode the list and download responses (success and
/// error arms), and the client-side decoders read them back exactly.
#[test]
fn host_serves_the_get_exchange_with_public_codecs() {
    // Client asks for a list (wants=None) — host decodes it.
    let list_req = MessageGetRequest::list();
    let decoded = MessageGetRequest::decode(&list_req.encode().expect("encode")).expect("decode");
    assert_eq!(decoded, list_req);

    // Host answers with transient IDs — client decodes them back.
    let ids = vec![[0x01u8; 32], [0x02u8; 32]];
    let listing = MessageListResponse::TransientIds(ids.clone());
    let wire = listing.encode().expect("encode");
    assert_eq!(
        MessageListResponse::decode(&wire).expect("decode"),
        MessageListResponse::TransientIds(ids)
    );

    // Host download response, success and error arms.
    let msgs = vec![vec![0xAA; 40], vec![0xBB; 12]];
    let get = MessageGetResponse::Messages(msgs.clone());
    assert_eq!(
        MessageGetResponse::decode(&get.encode().expect("encode")).expect("decode"),
        MessageGetResponse::Messages(msgs)
    );
    let err = MessageListResponse::Error(PeerError::Throttled);
    assert_eq!(
        MessageListResponse::decode(&err.encode().expect("encode")).expect("decode"),
        MessageListResponse::Error(PeerError::Throttled)
    );

    // Host announces itself — the client decoder reads it back.
    let ann = PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 1_700_000_000,
        enabled: true,
        transfer_limit_kb: 1024,
        sync_limit_kb: 4096,
        stamp_cost: 12,
        stamp_cost_flexibility: 2,
        peering_cost: 0,
        metadata: Vec::new(),
    };
    let wire = ann.encode().expect("encode");
    assert_eq!(PropagationNodeAnnounce::decode(&wire).expect("decode"), ann);
}
