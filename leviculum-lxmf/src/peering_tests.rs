//! Unit tests for the peering protocol half (Codeberg #384 part 2).

use super::*;
use alloc::vec;

fn announce(timebase: u64) -> PropagationNodeAnnounce {
    PropagationNodeAnnounce {
        legacy_support: false,
        timebase,
        enabled: true,
        transfer_limit_kb: 4,
        sync_limit_kb: 32,
        stamp_cost: 16,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        metadata: vec![],
    }
}

fn table() -> PeerTable {
    PeerTable::new(PeeringConfig::default())
}

fn entry(sequence: u64, size: u32, stamp_value: u8) -> StoredMessage {
    StoredMessage {
        transient_id: {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&sequence.to_be_bytes());
            id
        },
        destination_hash: [7; 16],
        size,
        received_at: 1000 + sequence,
        stamp_value,
        sequence,
    }
}

fn peer_at(cursor: u64) -> Peer {
    let mut peer = Peer::from_announce([1; 16], &announce(1), false, 0);
    peer.cursor = cursor;
    peer
}

// ---- peer table ----

#[test]
fn an_announce_inside_the_depth_creates_a_peer_and_a_newer_one_updates_it() {
    let mut table = table();
    assert_eq!(
        table.handle_announce([1; 16], &announce(100), Some(2), 50),
        PeerChange::Added
    );
    // Same timebase: liveness only, no config update (LXMRouter.py:2016).
    let mut cheaper = announce(100);
    cheaper.peering_cost = 3;
    assert_eq!(
        table.handle_announce([1; 16], &cheaper, Some(2), 60),
        PeerChange::Updated
    );
    assert_eq!(table.get(&[1; 16]).unwrap().peering_cost, 18);
    // Newer timebase: config follows the announce.
    let mut newer = announce(101);
    newer.peering_cost = 3;
    assert_eq!(
        table.handle_announce([1; 16], &newer, Some(2), 70),
        PeerChange::Updated
    );
    assert_eq!(table.get(&[1; 16]).unwrap().peering_cost, 3);
    assert_eq!(table.get(&[1; 16]).unwrap().last_heard, 70);
}

#[test]
fn the_cap_declines_new_peers_first_heard_wins() {
    let mut table = PeerTable::new(PeeringConfig {
        max_peers: 2,
        ..PeeringConfig::default()
    });
    assert_eq!(
        table.handle_announce([1; 16], &announce(1), Some(1), 0),
        PeerChange::Added
    );
    assert_eq!(
        table.handle_announce([2; 16], &announce(1), Some(1), 0),
        PeerChange::Added
    );
    assert_eq!(
        table.handle_announce([3; 16], &announce(1), Some(1), 0),
        PeerChange::Declined(DeclineReason::TableFull)
    );
    assert_eq!(table.len(), 2);
    // A freed slot re-opens the table (the deterministic policy: slots
    // free by cull/unpeer, not by announce-time eviction).
    assert!(table.remove(&[1; 16]));
    assert_eq!(
        table.handle_announce([3; 16], &announce(2), Some(1), 5),
        PeerChange::Added
    );
}

#[test]
fn a_static_peer_bypasses_the_cap_and_the_depth_and_survives_the_cull() {
    let mut table = PeerTable::new(PeeringConfig {
        max_peers: 1,
        static_peers: vec![[9; 16]],
        ..PeeringConfig::default()
    });
    assert_eq!(
        table.handle_announce([1; 16], &announce(1), Some(1), 0),
        PeerChange::Added
    );
    // Beyond depth and over the cap, still peered: static.
    assert_eq!(
        table.handle_announce([9; 16], &announce(1), Some(9), 0),
        PeerChange::Added
    );
    let dropped = table.cull(MAX_UNREACHABLE_SECS + 10);
    assert_eq!(dropped, vec![[1; 16]]);
    assert!(table.get(&[9; 16]).is_some());
}

#[test]
fn out_of_depth_and_disabled_and_costly_announces_break_or_decline() {
    let mut table = table();
    assert_eq!(
        table.handle_announce([1; 16], &announce(1), Some(5), 0),
        PeerChange::Declined(DeclineReason::TooDeep)
    );
    table.handle_announce([1; 16], &announce(1), Some(4), 0);
    assert_eq!(
        table.handle_announce([1; 16], &announce(2), Some(5), 1),
        PeerChange::Dropped(DropReason::OutOfDepth)
    );

    let mut disabled = announce(3);
    disabled.enabled = false;
    table.handle_announce([2; 16], &announce(1), Some(1), 0);
    assert_eq!(
        table.handle_announce([2; 16], &disabled, Some(1), 1),
        PeerChange::Dropped(DropReason::Disabled)
    );

    let mut costly = announce(4);
    costly.peering_cost = 27;
    assert_eq!(
        table.handle_announce([3; 16], &costly, Some(1), 0),
        PeerChange::Declined(DeclineReason::CostTooHigh)
    );
    table.handle_announce([3; 16], &announce(4), Some(1), 0);
    assert_eq!(
        table.handle_announce([3; 16], &costly, Some(1), 1),
        PeerChange::Dropped(DropReason::CostRaised)
    );
}

#[test]
fn peering_key_readiness_follows_the_announced_cost() {
    let mut peer = peer_at(0);
    assert!(!peer.peering_key_ready());
    peer.peering_key = Some(([0xAA; 32], 17));
    // Value below the announced cost 18: discarded for re-mining
    // (LXMPeer.py:230-234).
    assert!(!peer.peering_key_ready());
    assert_eq!(peer.peering_key, None);
    peer.peering_key = Some(([0xAA; 32], 18));
    assert!(peer.peering_key_ready());
    // Cost 0 (our deviation from the reference's falsy-zero dead end):
    // any key is ready.
    peer.peering_cost = 0;
    peer.peering_key = Some(([0xAA; 32], 0));
    assert!(peer.peering_key_ready());
}

#[test]
fn next_due_walks_hash_order_and_honours_backoff_and_cursor() {
    let mut table = table();
    table.handle_announce([1; 16], &announce(1), Some(1), 0);
    table.handle_announce([2; 16], &announce(1), Some(1), 0);
    table.handle_announce([3; 16], &announce(1), Some(1), 0);
    table.get_mut(&[1; 16]).unwrap().cursor = 5; // caught up
    table.get_mut(&[2; 16]).unwrap().next_sync_attempt = 100; // backing off

    assert_eq!(table.next_due(50, 5, None), Some([3; 16]));
    // Past the backoff, [2] is due and rotation starts after the last.
    assert_eq!(table.next_due(150, 5, Some([3; 16])), Some([2; 16]));
    // Everyone caught up: nothing due.
    table.get_mut(&[2; 16]).unwrap().cursor = 5;
    table.get_mut(&[3; 16]).unwrap().cursor = 5;
    assert_eq!(table.next_due(150, 5, None), None);
}

#[test]
fn max_peer_min_cost_drives_the_accept_time_value_policy() {
    let mut table = table();
    assert_eq!(table.max_peer_min_cost(), 0);
    table.handle_announce([1; 16], &announce(1), Some(1), 0);
    // 16 − 3 (announce fixture) = 13.
    assert_eq!(table.max_peer_min_cost(), 13);
}

// ---- wire codecs ----

#[test]
fn offer_round_trips_and_matches_the_msgpack_shape_python_unpacks() {
    let offer = PeerOffer {
        peering_key: [0xAB; 32],
        transient_ids: vec![[1; 32], [2; 32]],
    };
    let encoded = offer.encode();
    // [key, [id, id]]: fixarray(2), bin8(32), fixarray(2), bin8(32) × 2 —
    // exactly what umsgpack produces for [bytes, [bytes, bytes]] and what
    // offer_request indexes as data[0] / data[1] (LXMRouter.py:2298-2304).
    assert_eq!(encoded[0], 0x92);
    assert_eq!(&encoded[1..3], &[0xC4, 0x20]);
    assert_eq!(encoded[35], 0x92);
    assert_eq!(&encoded[36..38], &[0xC4, 0x20]);
    assert_eq!(encoded.len(), 3 + 32 + 1 + 2 * 34);
    assert_eq!(PeerOffer::decode(&encoded).unwrap(), offer);
}

#[test]
fn offer_responses_round_trip_in_all_four_shapes() {
    for response in [
        OfferResponse::WantNone,
        OfferResponse::WantAll,
        OfferResponse::Wanted(vec![[3; 32]]),
        OfferResponse::Error(PeerError::Throttled),
    ] {
        let encoded = response.encode();
        assert_eq!(OfferResponse::decode(&encoded).unwrap(), response);
    }
    // The exact byte shapes the reference emits: `False`/`True` are the
    // msgpack booleans, an error is the bare uint (offer_response
    // compares `response == False` etc., LXMPeer.py:408-450).
    assert_eq!(OfferResponse::WantNone.encode(), vec![0xC2]);
    assert_eq!(OfferResponse::WantAll.encode(), vec![0xC3]);
    assert_eq!(
        OfferResponse::Error(PeerError::Throttled).encode(),
        vec![0xCC, 0xF6]
    );
}

#[test]
fn the_sync_envelope_is_the_multi_message_upload_shape() {
    let envelope = PeerSyncEnvelope {
        timestamp: 1234.5,
        messages: vec![vec![1u8; 150], vec![2u8; 150]],
    };
    let encoded = envelope.encode();
    let decoded = PeerSyncEnvelope::decode(&encoded).unwrap();
    assert_eq!(decoded, envelope);
    // The single-message form decodes too; the peering-key gate on the
    // multi form is the caller's, as it is in the reference
    // (LXMRouter.py:2381-2389).
    let single = PeerSyncEnvelope {
        timestamp: 1.0,
        messages: vec![vec![3u8; 150]],
    };
    assert_eq!(
        PeerSyncEnvelope::decode(&single.encode())
            .unwrap()
            .messages
            .len(),
        1
    );
}

#[test]
fn peering_key_material_puts_the_validator_first() {
    // key_material = peer.identity.hash + router.identity.hash on the
    // mining side (LXMPeer.py:258); peering_id = identity.hash +
    // remote_identity.hash on the validating side (LXMRouter.py:2300).
    let material = peering_key_material(&[1; 16], &[2; 16]);
    assert_eq!(&material[..16], &[1; 16]);
    assert_eq!(&material[16..], &[2; 16]);
}

// ---- inbound ----

#[test]
fn answer_offer_wants_only_what_the_store_lacks() {
    let held = [5u8; 32];
    let missing_a = [6u8; 32];
    let missing_b = [7u8; 32];
    assert_eq!(
        answer_offer(&[held], |id| *id == held),
        OfferResponse::WantNone
    );
    assert_eq!(
        answer_offer(&[missing_a, missing_b], |id| *id == held),
        OfferResponse::WantAll
    );
    assert_eq!(
        answer_offer(&[held, missing_a], |id| *id == held),
        OfferResponse::Wanted(vec![missing_a])
    );
}

#[test]
fn the_inbound_gate_applies_the_reference_order_and_static_bypass() {
    let config = PeeringConfig {
        max_inbound_syncs: 1,
        from_static_only: false,
        static_peers: vec![[9; 16]],
        ..PeeringConfig::default()
    };
    let mut gate = InboundGate::default();
    // Sequential validation running: throttled (LXMRouter.py:2274-2278).
    assert_eq!(
        gate.admit(&config, &[1; 16], 0, true, 0),
        Err(PeerError::Throttled)
    );
    // At the inbound-sync cap: throttled (:2280-2284).
    assert_eq!(
        gate.admit(&config, &[1; 16], 0, false, 1),
        Err(PeerError::Throttled)
    );
    // A static peer bypasses both (:2273).
    assert_eq!(gate.admit(&config, &[9; 16], 0, true, 5), Ok(()));
    // The per-peer throttle expires (:2286-2291).
    gate.throttle([1; 16], 100);
    assert_eq!(
        gate.admit(&config, &[1; 16], 100, false, 0),
        Err(PeerError::Throttled)
    );
    assert!(gate.is_throttled(&[1; 16], 100 + PN_STAMP_THROTTLE_SECS - 1));
    assert_eq!(
        gate.admit(&config, &[1; 16], 100 + PN_STAMP_THROTTLE_SECS, false, 0),
        Ok(())
    );
    // from_static_only refuses strangers with NO_ACCESS (:2292-2295).
    let static_only = PeeringConfig {
        from_static_only: true,
        static_peers: vec![[9; 16]],
        ..PeeringConfig::default()
    };
    assert_eq!(
        gate.admit(&static_only, &[1; 16], 500, false, 0),
        Err(PeerError::NoAccess)
    );
    assert_eq!(gate.admit(&static_only, &[9; 16], 500, false, 0), Ok(()));
}

// ---- outbound offer building ----

#[test]
fn build_offer_takes_everything_above_the_cursor_in_order() {
    let peer = peer_at(1);
    let entries = [entry(1, 300, 16), entry(2, 300, 16), entry(3, 300, 16)];
    let plan = build_offer(&peer, &entries).unwrap();
    assert_eq!(
        plan.ids,
        vec![entries[1].transient_id, entries[2].transient_id]
    );
    assert_eq!(plan.cursor_target, 3);
}

#[test]
fn low_value_and_oversize_entries_are_skipped_forever_via_the_cursor() {
    // Peer requires stamp value ≥ 13 (announce fixture); transfer limit
    // 4 kB.
    let peer = peer_at(0);
    let entries = [
        entry(1, 300, 0),   // below the peer's minimum (LXMPeer.py:340)
        entry(2, 300, 16),  // offered
        entry(3, 5000, 16), // above 4 kB transfer limit (LXMPeer.py:370)
        entry(4, 300, 16),  // offered
    ];
    let plan = build_offer(&peer, &entries).unwrap();
    assert_eq!(
        plan.ids,
        vec![entries[1].transient_id, entries[3].transient_id]
    );
    assert_eq!(plan.cursor_target, 4);
    assert_eq!(plan.skipped_low_value, 1);
    assert_eq!(plan.skipped_oversize, 1);
}

#[test]
fn the_sync_limit_stops_the_round_without_advancing_past_the_rest() {
    // Sync limit 32 kB (announce fixture): 24 + n×(3900+16) < 32000
    // admits 8 (each stays under the 4 kB per-message limit).
    let peer = peer_at(0);
    let entries: Vec<StoredMessage> = (1..=10).map(|n| entry(n, 3900, 16)).collect();
    let plan = build_offer(&peer, &entries).unwrap();
    assert_eq!(plan.ids.len(), 8);
    // The cursor stops at the last offered entry: nothing resumable was
    // stepped past.
    assert_eq!(plan.cursor_target, 8);
}

#[test]
fn the_offer_byte_budget_bounds_one_round() {
    let mut peer = peer_at(0);
    peer.sync_limit_kb = 10_240; // out of the way (reference SYNC_LIMIT)
    let entries: Vec<StoredMessage> = (1..=300).map(|n| entry(n, 10, 16)).collect();
    let plan = build_offer(&peer, &entries).unwrap();
    // (6144 − 40) / 34 = 179 ids fit the §5 offer budget.
    assert_eq!(plan.ids.len(), 179);
    assert_eq!(plan.cursor_target, 179);
    assert!(plan.ids.len() * 34 + 40 <= OFFER_BYTES_LIMIT);
}

#[test]
fn a_stale_cursor_reads_as_older_than_everything_live() {
    // The reclaimed-page / reset case: cursor 0 against a store whose
    // sequences begin far above it — the full bounded re-offer.
    let peer = peer_at(0);
    let entries = [entry(900, 300, 16), entry(901, 300, 16)];
    let plan = build_offer(&peer, &entries).unwrap();
    assert_eq!(plan.ids.len(), 2);
    assert_eq!(plan.cursor_target, 901);
}

#[test]
fn nothing_to_offer_is_none_but_pure_skips_still_advance() {
    let peer = peer_at(5);
    assert_eq!(build_offer(&peer, &[entry(5, 300, 16)]), None);
    // Only-skippable content still yields a plan whose empty offer moves
    // the cursor (otherwise the same dead entries scan forever).
    let plan = build_offer(&peer, &[entry(6, 300, 0)]).unwrap();
    assert!(plan.ids.is_empty());
    assert_eq!(plan.cursor_target, 6);
}

// ---- response mapping ----

#[test]
fn response_actions_match_the_reference_semantics() {
    let plan = OfferPlan {
        ids: vec![[1; 32], [2; 32]],
        cursor_target: 7,
        skipped_oversize: 0,
        skipped_low_value: 0,
    };
    assert_eq!(
        response_action(&OfferResponse::WantNone, &plan),
        ResponseAction::Concluded
    );
    assert_eq!(
        response_action(&OfferResponse::WantAll, &plan),
        ResponseAction::SendMessages(vec![[1; 32], [2; 32]])
    );
    // Only offered ids come back; a stranger id in the response is not
    // served.
    assert_eq!(
        response_action(&OfferResponse::Wanted(vec![[2; 32], [9; 32]]), &plan),
        ResponseAction::SendMessages(vec![[2; 32]])
    );
    assert_eq!(
        response_action(&OfferResponse::Error(PeerError::Throttled), &plan),
        ResponseAction::Backoff(PN_STAMP_THROTTLE_SECS)
    );
    assert_eq!(
        response_action(&OfferResponse::Error(PeerError::NoAccess), &plan),
        ResponseAction::Unpeer
    );
    assert_eq!(
        response_action(&OfferResponse::Error(PeerError::InvalidKey), &plan),
        ResponseAction::RemineKey
    );
}

// ---- persistence ----

#[test]
fn peer_records_round_trip_through_a_store_and_restore() {
    let mut peer = peer_at(42);
    peer.identity_hash = Some([8; 16]);
    peer.peering_key = Some(([0xEE; 32], 19));
    let mut store = MemoryPeerStore::default();
    store.save(&PeerRecord::of(&peer)).unwrap();

    let mut table = table();
    table.restore(store.load_all().unwrap());
    let restored = table.get(&peer.destination_hash).unwrap();
    assert_eq!(restored.cursor, 42);
    assert_eq!(restored.peering_key, Some(([0xEE; 32], 19)));
    assert_eq!(restored.identity_hash, Some([8; 16]));
    assert_eq!(restored.state, SyncPhase::Idle);

    store.remove(&peer.destination_hash).unwrap();
    assert!(store.load_all().unwrap().is_empty());
}

// ---- identity recall survives the identity-cache roll (#388 pass 3) ----

/// Sixteen peers announced, then nine unrelated identities announced —
/// the board's 8-slot `known_identities` cache has rolled twice over —
/// and a sync toward each of the sixteen still finds its keys (from the
/// copy captured with the peer) and opens its link.
#[test]
fn peer_keys_survive_identity_cache_roll() {
    use leviculum_core::traits::{Clock, NoStorage};
    use leviculum_core::{DestinationHash, EmbeddedStorage, NodeCoreBuilder};
    use rand_core::OsRng;

    struct TestClock;
    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            0
        }
    }

    let mut storage = EmbeddedStorage::new();
    let mut peers = PeerTable::new(PeeringConfig {
        max_peers: 16,
        ..PeeringConfig::default()
    });

    // Sixteen announces: each puts the identity into the cache (as the
    // core's announce processing does) and is captured with the peer (as
    // the role does in its announce handler).
    let mut identities = alloc::vec::Vec::new();
    for n in 0..16u8 {
        let dest = [n; 16];
        let identity = Identity::generate(&mut OsRng);
        storage.set_identity(dest, identity.clone());
        peers.handle_announce(dest, &announce(1), Some(1), 0);
        let cached = storage.get_identity(&dest).cloned().expect("just cached");
        peers
            .get_mut(&dest)
            .expect("announce peered")
            .capture_identity(&cached);
        identities.push((dest, identity));
    }

    // Nine unrelated identities roll the 8-slot cache; every peer's
    // cache entry is gone.
    for n in 100..109u8 {
        storage.set_identity([n; 16], Identity::generate(&mut OsRng));
    }
    for (dest, _) in &identities {
        assert!(
            storage.get_identity(dest).is_none(),
            "cache must have rolled past peer {dest:?}"
        );
    }

    // The sync round toward each of the sixteen still finds keys via the
    // peer-first recall order and opens its link with them.
    let mut node = NodeCoreBuilder::new().build(OsRng, TestClock, NoStorage);
    for (dest, original) in &identities {
        let recalled = peers
            .get(dest)
            .expect("still peered")
            .recall_identity(&storage)
            .expect("keys kept with the peer");
        assert_eq!(recalled.hash(), original.hash(), "identity hash matches");
        assert_eq!(
            recalled.ed25519_verifying().to_bytes(),
            original.ed25519_verifying().to_bytes(),
            "signing key matches — the proof this key verifies is the peer's"
        );
        let signing = recalled.ed25519_verifying().to_bytes();
        let _out = node
            .connect(DestinationHash::new(*dest), &signing)
            .expect("link request goes out on the recalled key");
    }
}

/// The captured keys ride the persisted record: a restore recalls the
/// identity without any cache at all.
#[test]
fn captured_keys_survive_store_restore() {
    use leviculum_core::traits::NoStorage;
    use rand_core::OsRng;

    let identity = Identity::generate(&mut OsRng);
    let mut peer = peer_at(0);
    assert!(peer.capture_identity(&identity));
    assert!(
        !peer.capture_identity(&identity),
        "second capture is a no-op"
    );

    let mut store = MemoryPeerStore::default();
    store.save(&PeerRecord::of(&peer)).unwrap();
    let mut restored = table();
    restored.restore(store.load_all().unwrap());
    let recalled = restored
        .get(&peer.destination_hash)
        .unwrap()
        .recall_identity(&NoStorage)
        .expect("recall needs no cache");
    assert_eq!(recalled.hash(), identity.hash());
}

// ---- announced sync limit bounds the wire resource (#388 pass 3, item 3) ----

/// Pack the largest legal batches the reference's loop admits and check
/// the REAL wire encoding (`msgpack([ts, [bin, ...]])`,
/// `LXMPeer.py:466`) against the announced limit. The loop counts
/// [`OFFER_BASE_SIZE`] up front plus [`OFFER_PER_MESSAGE_OVERHEAD`] per
/// message and stops strictly below the limit (`LXMPeer.py:359-360`,
/// `:376`); the wire framing must be dominated by that accounting for
/// every message-size mix, including the msgpack bin8/bin16 boundary.
#[test]
fn sync_batch_wire_size_never_exceeds_announced_limit() {
    const LIMIT: u64 = 8 * 1000; // the board's announced sync limit shape

    // The reference's packing rule: admit while
    // `cumulative + size + overhead < limit` (strict).
    fn pack_batch(message_size: usize) -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
        let mut batch = alloc::vec::Vec::new();
        let mut cumulative = OFFER_BASE_SIZE;
        loop {
            let transfer = message_size as u64 + OFFER_PER_MESSAGE_OVERHEAD;
            if cumulative + transfer >= LIMIT {
                return batch;
            }
            cumulative += transfer;
            batch.push(vec![0xAB; message_size]);
        }
    }

    fn wire_bytes(batch: &[alloc::vec::Vec<u8>]) -> usize {
        let mut out = alloc::vec::Vec::new();
        msgpack::array(&mut out, 2);
        msgpack::f64(&mut out, 1_726_000_000.5);
        msgpack::array(&mut out, batch.len());
        for message in batch {
            msgpack::bin(&mut out, message);
        }
        out.len()
    }

    // Message sizes chosen to stress the framing: the degenerate empty
    // message (maximum count, maximum framing share), both sides of the
    // bin8/bin16 header boundary, a realistic tiny message, and the
    // largest per-message size a 4 KB transfer limit admits.
    for message_size in [0usize, 1, 100, 255, 256, 4000] {
        let batch = pack_batch(message_size);
        assert!(!batch.is_empty(), "size {message_size}: batch must pack");
        let wire = wire_bytes(&batch);
        assert!(
            (wire as u64) < LIMIT,
            "size {message_size}: wire {wire} B (n={}) must stay below the \
             announced limit {LIMIT}",
            batch.len()
        );
    }
}
