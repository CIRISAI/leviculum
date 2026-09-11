//! Unit tests for the propagation-node role over the in-memory store.
//!
//! Byte-exact wire fixtures live in `tests/propagation_node.rs`, generated
//! from the vendored Python reference by
//! `docs/src/appendix/lxmf/vectors/gen_vectors.py`; these tests pin the
//! role's *behaviour* — dedup, limits, eviction, purge-on-confirmation —
//! with hand-built envelopes.

use super::*;
use crate::propagation_store::MemoryPropagationStore;
use alloc::vec;
use leviculum_core::crypto::full_hash;

const DAY: u64 = 24 * 60 * 60;

fn node(capacity: u64) -> PropagationNode<MemoryPropagationStore> {
    PropagationNode::new(
        MemoryPropagationStore::new(capacity),
        PropagationNodeConfig::default(),
    )
}

/// A syntactically valid upload envelope for `destination`, unique per
/// `seed`, with an arbitrary stamp (the default config costs 0).
fn envelope(destination: u8, seed: u8) -> Vec<u8> {
    envelope_sized(destination, seed, 120)
}

fn envelope_sized(destination: u8, seed: u8, lxmf_len: usize) -> Vec<u8> {
    assert!(lxmf_len > crate::constants::LXMF_OVERHEAD);
    let mut lxmf_data = vec![destination; DESTINATION_LENGTH];
    lxmf_data.resize(lxmf_len, seed);
    PropagationUpload::single(1_700_000_000.0, lxmf_data, [0xEE; STAMP_SIZE]).encode()
}

fn no_validation(_: &TransientId, _: &[u8; STAMP_SIZE]) -> Option<u16> {
    panic!("stamp validation must not run at cost 0");
}

fn accepted_id(outcome: &UploadOutcome) -> TransientId {
    match outcome {
        UploadOutcome::Accepted { transient_id, .. } => *transient_id,
        other => panic!("expected acceptance, got {other:?}"),
    }
}

#[test]
fn an_upload_is_stored_and_a_repeat_is_a_proven_duplicate() {
    let mut node = node(64 * 1024);
    let bytes = envelope(7, 1);

    let first = node.handle_upload(&bytes, 100, no_validation);
    let transient_id = accepted_id(&first);
    match &first {
        UploadOutcome::Accepted {
            destination_hash,
            duplicate,
            stamp_value,
            ..
        } => {
            assert_eq!(destination_hash, &[7u8; DESTINATION_LENGTH]);
            assert!(!duplicate);
            assert_eq!(*stamp_value, 0);
        }
        other => panic!("{other:?}"),
    }
    assert!(node.store().contains(&transient_id).unwrap());

    // The transient ID is SHA-256 over the unstamped bytes, before the
    // stamp (lxmf_propagation, reference/LXMF/LXMF/LXMRouter.py:2494).
    let stored = node.store().read_body(&transient_id).unwrap().unwrap();
    assert_eq!(
        full_hash(&stored[..stored.len() - STAMP_SIZE]),
        transient_id
    );

    // A duplicate is accepted (so the caller proves it) but not stored
    // twice.
    let again = node.handle_upload(&bytes, 200, no_validation);
    match again {
        UploadOutcome::Accepted { duplicate, .. } => assert!(duplicate),
        other => panic!("{other:?}"),
    }
    assert_eq!(node.store().len(), 1);
}

#[test]
fn a_multi_message_transfer_is_the_peer_sync_form() {
    let mut node = node(64 * 1024);
    // Hand-build [timestamp, [msg, msg]].
    let mut bytes = Vec::new();
    crate::msgpack::array(&mut bytes, 2);
    crate::msgpack::f64(&mut bytes, 0.0);
    crate::msgpack::array(&mut bytes, 2);
    crate::msgpack::bin(&mut bytes, &[0u8; 160]);
    crate::msgpack::bin(&mut bytes, &[1u8; 160]);
    assert_eq!(
        node.handle_upload(&bytes, 0, no_validation),
        UploadOutcome::PeerSyncForm
    );
}

#[test]
fn a_stamp_below_the_cost_is_rejected_with_the_python_signal() {
    let mut node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            stamp_cost: 8,
            stamp_cost_flexibility: 3,
            ..PropagationNodeConfig::default()
        },
    );
    assert_eq!(node.min_accepted_cost(), 5);

    let outcome = node.handle_upload(&envelope(7, 1), 0, |_, _| None);
    match outcome {
        UploadOutcome::InvalidStamp { reject } => {
            // The exact bytes Python sends: msgpack [ERROR_INVALID_STAMP]
            // (reference/LXMF/LXMF/LXMRouter.py:2258).
            assert_eq!(
                PropagationSignal::decode(&reject).unwrap(),
                PropagationSignal::InvalidStamp
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(node.store().is_empty());

    // And a validating stamp is stored with its value.
    let outcome = node.handle_upload(&envelope(7, 2), 0, |_, _| Some(6));
    match outcome {
        UploadOutcome::Accepted { stamp_value, .. } => assert_eq!(stamp_value, 6),
        other => panic!("{other:?}"),
    }
}

#[test]
fn get_list_returns_only_the_callers_mailbox_smallest_first() {
    let mut node = node(64 * 1024);
    let big = accepted_id(&node.handle_upload(&envelope_sized(7, 1, 300), 0, no_validation));
    let small = accepted_id(&node.handle_upload(&envelope_sized(7, 2, 120), 0, no_validation));
    let _other = node.handle_upload(&envelope(9, 3), 0, no_validation);

    let outcome = node
        .handle_get(
            &MessageGetRequest::list().encode().unwrap(),
            &[7; DESTINATION_LENGTH],
            0,
        )
        .unwrap();
    match outcome {
        GetOutcome::List { response, count } => {
            assert_eq!(count, 2);
            match MessageListResponse::decode(&response).unwrap() {
                MessageListResponse::TransientIds(ids) => assert_eq!(ids, vec![small, big]),
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fetch_serves_unstamped_bodies_and_deletes_only_on_confirmation() {
    let mut node = node(64 * 1024);
    let bytes = envelope(7, 1);
    let transient_id = accepted_id(&node.handle_upload(&bytes, 0, no_validation));
    let stored = node.store().read_body(&transient_id).unwrap().unwrap();

    // Fetch: the body arrives without the trailing stamp, and nothing is
    // deleted by the fetch itself.
    let fetch = MessageGetRequest {
        wants: Some(vec![transient_id]),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1000)),
    };
    let outcome = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    match outcome {
        GetOutcome::Fetch {
            response,
            served,
            purged,
            ..
        } => {
            assert_eq!(served, vec![transient_id]);
            assert!(purged.is_empty());
            match MessageGetResponse::decode(&response).unwrap() {
                MessageGetResponse::Messages(bodies) => {
                    assert_eq!(bodies, vec![stored[..stored.len() - STAMP_SIZE].to_vec()]);
                }
                other => panic!("{other:?}"),
            }
        }
        other => panic!("{other:?}"),
    }
    assert!(node.store().contains(&transient_id).unwrap());

    // The explicit confirmation deletes, and the store is empty after it.
    let acknowledge = MessageGetRequest::acknowledge(vec![transient_id]);
    let outcome = node
        .handle_get(&acknowledge.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    match outcome {
        GetOutcome::Fetch { served, purged, .. } => {
            assert!(served.is_empty());
            assert_eq!(purged, vec![transient_id]);
        }
        other => panic!("{other:?}"),
    }
    assert!(node.store().is_empty());

    // A retry of the original upload after the drain is a duplicate, not a
    // re-acceptance (locally_processed_transient_ids,
    // reference/LXMF/LXMF/LXMRouter.py:2496).
    match node.handle_upload(&bytes, 10, no_validation) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(duplicate),
        other => panic!("{other:?}"),
    }
    assert!(node.store().is_empty());
}

#[test]
fn a_stranger_cannot_fetch_or_purge_someone_elses_mail() {
    let mut node = node(64 * 1024);
    let transient_id = accepted_id(&node.handle_upload(&envelope(7, 1), 0, no_validation));

    let steal = MessageGetRequest {
        wants: Some(vec![transient_id]),
        haves: Some(vec![transient_id]),
        transfer_limit_kb: None,
    };
    let outcome = node
        .handle_get(&steal.encode().unwrap(), &[9; DESTINATION_LENGTH], 0)
        .unwrap();
    match outcome {
        GetOutcome::Fetch { served, purged, .. } => {
            assert!(served.is_empty(), "wrong mailbox must not be served");
            assert!(purged.is_empty(), "wrong mailbox must not purge");
        }
        other => panic!("{other:?}"),
    }
    assert!(node.store().contains(&transient_id).unwrap());
}

#[test]
fn the_transfer_limit_bounds_one_response_and_skips_rather_than_stops() {
    let mut node = node(64 * 1024);
    let big = accepted_id(&node.handle_upload(&envelope_sized(7, 1, 1000), 0, no_validation));
    let small = accepted_id(&node.handle_upload(&envelope_sized(7, 2, 120), 0, no_validation));

    // 1 kB budget: the 1000-byte body plus its 32-byte stamp and the 24 + 16
    // structural overhead (reference/LXMF/LXMF/LXMRouter.py:1532-1533)
    // exceeds it, the small one after it still fits — the reference skips
    // and keeps scanning (reference/LXMF/LXMF/LXMRouter.py:1547).
    let fetch = MessageGetRequest {
        wants: Some(vec![big, small]),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1)),
    };
    let outcome = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    match outcome {
        GetOutcome::Fetch { served, .. } => assert_eq!(served, vec![small]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn expiry_purges_only_what_is_older_than_thirty_days() {
    let mut node = node(64 * 1024);
    let old = accepted_id(&node.handle_upload(&envelope(7, 1), 0, no_validation));
    let fresh = accepted_id(&node.handle_upload(&envelope(7, 2), 20 * DAY, no_validation));

    let evicted = node.tick(31 * DAY);
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].transient_id, old);
    assert_eq!(evicted[0].reason, EvictionReason::Expired);
    assert!(!node.store().contains(&old).unwrap());
    assert!(node.store().contains(&fresh).unwrap());
}

#[test]
fn a_full_store_displaces_by_age_times_size() {
    // Room for roughly two field-sized messages.
    let mut node = node(340);
    let oldest = accepted_id(&node.handle_upload(&envelope(7, 1), 0, no_validation));
    let newer = accepted_id(&node.handle_upload(&envelope(7, 2), 9 * DAY, no_validation));

    // Same size, so the weight is decided by age (get_weight,
    // reference/LXMF/LXMF/LXMRouter.py:1062): the oldest goes.
    let outcome = node.handle_upload(&envelope(7, 3), 10 * DAY, no_validation);
    match &outcome {
        UploadOutcome::Accepted {
            duplicate, evicted, ..
        } => {
            assert!(!duplicate);
            assert_eq!(evicted.len(), 1);
            assert_eq!(evicted[0].transient_id, oldest);
            assert_eq!(evicted[0].reason, EvictionReason::Displaced);
        }
        other => panic!("{other:?}"),
    }
    assert!(!node.store().contains(&oldest).unwrap());
    assert!(node.store().contains(&newer).unwrap());
    assert!(node.store().contains(&accepted_id(&outcome)).unwrap());
}

#[test]
fn the_resource_gate_follows_the_announced_sync_limit() {
    let node = node(64 * 1024);
    // Default field 4 is 32 (kilobytes of 1000 bytes).
    assert!(node.accepts_resource_of(32_000));
    assert!(!node.accepts_resource_of(32_001));
}

#[test]
fn announce_defaults_are_the_concept_papers_numbers() {
    let node = node(64 * 1024);
    let announce = PropagationNodeAnnounce::decode(&node.announce_app_data(1_700_000_000)).unwrap();
    assert!(!announce.legacy_support);
    assert!(announce.enabled);
    assert_eq!(announce.timebase, 1_700_000_000);
    assert_eq!(announce.transfer_limit_kb, 4);
    assert_eq!(announce.sync_limit_kb, 32);
    assert_eq!(announce.stamp_cost, 0);
    assert_eq!(announce.stamp_cost_flexibility, 3);
    assert_eq!(announce.peering_cost, 0);
    assert!(announce.metadata.is_empty());
}

/// The board's deferred-durability path: a store flush that failed after
/// the accept must be able to un-remember the id, so the client's retry is
/// stored rather than answered as a proven duplicate.
#[test]
fn a_forgotten_id_is_accepted_again_as_new() {
    let mut node = node(64 * 1024);
    let bytes = envelope(7, 1);
    let transient_id = accepted_id(&node.handle_upload(&bytes, 100, no_validation));

    // Simulate the failed flush: the body never became durable.
    assert!(node.store_mut().purge(&transient_id).unwrap());
    node.forget_processed(&transient_id);

    match node.handle_upload(&bytes, 101, no_validation) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(!duplicate),
        other => panic!("expected acceptance, got {other:?}"),
    }
}
