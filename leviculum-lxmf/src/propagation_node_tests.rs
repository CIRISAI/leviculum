//! Unit tests for the propagation-node role over the in-memory store.
//!
//! Byte-exact wire fixtures live in `tests/propagation_node.rs`, generated
//! from the vendored Python reference by
//! `docs/src/appendix/lxmf/vectors/gen_vectors.py`; these tests pin the
//! role's *behaviour* — dedup, limits, eviction, purge-on-confirmation —
//! with hand-built envelopes.

use super::*;
use crate::propagation::MessageListResponse;
use crate::propagation_store::{MemoryPropagationStore, MIN_BODY_LEN};
use alloc::rc::Rc;
use alloc::vec;
use core::cell::RefCell;
use leviculum_core::crypto::full_hash;

const DAY: u64 = 24 * 60 * 60;

/// The default config at stamp cost 0, so the behaviour tests below
/// exercise dedup, limits, eviction and serving without grinding stamps.
/// The shipped default costs 13
/// ([`the_default_config_announces_the_decided_costs`]).
fn cost_free() -> PropagationNodeConfig {
    PropagationNodeConfig {
        stamp_cost: 0,
        ..PropagationNodeConfig::default()
    }
}

fn node(capacity: u64) -> PropagationNode<MemoryPropagationStore> {
    PropagationNode::new(MemoryPropagationStore::new(capacity), cost_free())
}

/// A syntactically valid upload envelope for `destination`, unique per
/// `seed`, with an arbitrary stamp (the [`node`] helper costs 0).
fn envelope(destination: u8, seed: u8) -> Vec<u8> {
    envelope_sized(destination, seed, 120)
}

fn envelope_sized(destination: u8, seed: u8, lxmf_len: usize) -> Vec<u8> {
    assert!(lxmf_len > crate::constants::LXMF_OVERHEAD);
    let mut lxmf_data = vec![destination; DESTINATION_LENGTH];
    lxmf_data.resize(lxmf_len, seed);
    PropagationUpload::single(1_700_000_000.0, lxmf_data, [0xEE; STAMP_SIZE]).encode()
}

/// The validator at cost 0, where any stamp is valid: a host measures the
/// true value of every stamp (leviculum#442 D6), and these placeholder
/// stamps measure to 0.
fn stamp_of_value_zero(_: &TransientId, _: &[u8; STAMP_SIZE]) -> Option<u16> {
    Some(0)
}

/// The board's validator at cost 0 with no demanding peer: never called.
fn no_validation(_: &TransientId, _: &[u8; STAMP_SIZE]) -> Option<u16> {
    panic!("a board must not validate at cost 0 while no peer demands a value");
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

    let first = node.handle_upload(&bytes, 100, stamp_of_value_zero);
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
    let again = node.handle_upload(&bytes, 200, stamp_of_value_zero);
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
        node.handle_upload(&bytes, 0, stamp_of_value_zero),
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
    let big = accepted_id(&node.handle_upload(&envelope_sized(7, 1, 300), 0, stamp_of_value_zero));
    let small =
        accepted_id(&node.handle_upload(&envelope_sized(7, 2, 120), 0, stamp_of_value_zero));
    let _other = node.handle_upload(&envelope(9, 3), 0, stamp_of_value_zero);

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
    let transient_id = accepted_id(&node.handle_upload(&bytes, 0, stamp_of_value_zero));
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
        GetOutcome::Fetch { plan, purged } => {
            assert_eq!(plan.served_ids(), vec![transient_id]);
            assert!(purged.is_empty());
            let response = node.encode_fetch(&plan).unwrap();
            assert_eq!(response.len(), plan.encoded_len());
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
        GetOutcome::Fetch { plan, purged } => {
            assert!(plan.is_empty());
            assert_eq!(purged, vec![transient_id]);
        }
        other => panic!("{other:?}"),
    }
    assert!(node.store().is_empty());

    // A retry of the original upload after the drain is a duplicate, not a
    // re-acceptance (locally_processed_transient_ids,
    // reference/LXMF/LXMF/LXMRouter.py:2496).
    match node.handle_upload(&bytes, 10, stamp_of_value_zero) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(duplicate),
        other => panic!("{other:?}"),
    }
    assert!(node.store().is_empty());
}

#[test]
fn a_stranger_cannot_fetch_or_purge_someone_elses_mail() {
    let mut node = node(64 * 1024);
    let transient_id = accepted_id(&node.handle_upload(&envelope(7, 1), 0, stamp_of_value_zero));

    let steal = MessageGetRequest {
        wants: Some(vec![transient_id]),
        haves: Some(vec![transient_id]),
        transfer_limit_kb: None,
    };
    let outcome = node
        .handle_get(&steal.encode().unwrap(), &[9; DESTINATION_LENGTH], 0)
        .unwrap();
    match outcome {
        GetOutcome::Fetch { plan, purged } => {
            assert!(plan.is_empty(), "wrong mailbox must not be served");
            assert!(purged.is_empty(), "wrong mailbox must not purge");
        }
        other => panic!("{other:?}"),
    }
    assert!(node.store().contains(&transient_id).unwrap());
}

#[test]
fn the_transfer_limit_bounds_one_response_and_skips_rather_than_stops() {
    let mut node = node(64 * 1024);
    let big = accepted_id(&node.handle_upload(&envelope_sized(7, 1, 1000), 0, stamp_of_value_zero));
    let small =
        accepted_id(&node.handle_upload(&envelope_sized(7, 2, 120), 0, stamp_of_value_zero));

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
        GetOutcome::Fetch { plan, .. } => assert_eq!(plan.served_ids(), vec![small]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn expiry_purges_only_what_is_older_than_thirty_days() {
    let mut node = node(64 * 1024);
    let old = accepted_id(&node.handle_upload(&envelope(7, 1), 0, stamp_of_value_zero));
    let fresh = accepted_id(&node.handle_upload(&envelope(7, 2), 20 * DAY, stamp_of_value_zero));

    let evicted = node.tick(31 * DAY);
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].transient_id, old);
    assert_eq!(evicted[0].reason, EvictionReason::Expired);
    assert!(!node.store().contains(&old).unwrap());
    assert!(node.store().contains(&fresh).unwrap());
}

/// LV-1003: `knows` is the one reading behind both the ingest's duplicate
/// test and the `/offer` answer. A message held is known, and so is one
/// collected and purged; one never seen is not.
#[test]
fn a_collected_message_stays_known_after_it_left_the_store() {
    let mut node = node(64 * 1024);
    let held = accepted_id(&node.handle_upload(&envelope(7, 1), 100, stamp_of_value_zero));
    let collected = accepted_id(&node.handle_upload(&envelope(7, 2), 100, stamp_of_value_zero));
    assert!(node.store_mut().purge(&collected).unwrap());
    node.note_confirmed_deletion(&collected, 200);

    assert!(node.knows(&held, 300), "held in the store");
    assert!(!node.store().contains(&collected).unwrap());
    assert!(node.knows(&collected, 300), "collected and purged");
    assert!(!node.knows(&[0x99; 32], 300), "never seen");

    // The ingest agrees: offered again, it is a duplicate.
    match node.handle_upload(&envelope(7, 2), 300, stamp_of_value_zero) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(duplicate),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_full_store_displaces_by_age_times_size() {
    // Room for roughly two field-sized messages.
    let mut node = node(340);
    let oldest = accepted_id(&node.handle_upload(&envelope(7, 1), 0, stamp_of_value_zero));
    let newer = accepted_id(&node.handle_upload(&envelope(7, 2), 9 * DAY, stamp_of_value_zero));

    // Same size, so the weight is decided by age (get_weight,
    // reference/LXMF/LXMF/LXMRouter.py:1062): the oldest goes.
    let outcome = node.handle_upload(&envelope(7, 3), 10 * DAY, stamp_of_value_zero);
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

/// A prioritised destination's message outlives a heavier-by-age one of
/// another destination: its weight is a tenth (`get_weight`,
/// `reference/LXMF/LXMF/LXMRouter.py:1064`). Without the list the same
/// store culls it first, being the oldest (order 1294).
#[test]
fn a_prioritised_destination_is_culled_last() {
    let run = |prioritised: Vec<[u8; DESTINATION_LENGTH]>| {
        let mut node = PropagationNode::new(
            MemoryPropagationStore::new(340),
            PropagationNodeConfig {
                prioritised_destinations: prioritised,
                ..cost_free()
            },
        );
        let kept = accepted_id(&node.handle_upload(&envelope(7, 1), 0, stamp_of_value_zero));
        let other = accepted_id(&node.handle_upload(&envelope(9, 2), 9 * DAY, stamp_of_value_zero));
        let _ = node.handle_upload(&envelope(9, 3), 10 * DAY, stamp_of_value_zero);
        (
            node.store().contains(&kept).unwrap(),
            node.store().contains(&other).unwrap(),
        )
    };
    assert_eq!(run(vec![[7; DESTINATION_LENGTH]]), (true, false));
    assert_eq!(run(Vec::new()), (false, true), "the control: oldest goes");
}

/// A message bigger than the whole store fails without culling: every
/// eviction would be for nothing, and the store would be empty after
/// (the review's suspected fault, order 1294).
#[test]
fn a_body_larger_than_the_store_evicts_nothing() {
    let mut node = node(340);
    let held = accepted_id(&node.handle_upload(&envelope(7, 1), 0, stamp_of_value_zero));
    let outcome = node.handle_upload(&envelope_sized(7, 2, 400), DAY, stamp_of_value_zero);
    assert!(
        matches!(outcome, UploadOutcome::StoreFailed(StorageError::Full)),
        "{outcome:?}"
    );
    assert!(node.store().contains(&held).unwrap(), "nothing culled");
}

#[test]
fn the_resource_gate_follows_the_announced_sync_limit() {
    // Field 4 named, 32 (kilobytes of 1000 bytes), under what the store
    // holds, so the announced limit is the one the gate applies.
    let node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            sync_limit_kb: 32,
            ..cost_free()
        },
    );
    assert!(node.accepts_resource_of(32_000));
    assert!(!node.accepts_resource_of(32_001));
}

/// A host node that names no limit announces lxmd's, `PROPAGATION_LIMIT`
/// 256 and `SYNC_LIMIT` 40 times that (`reference/LXMF/LXMF/LXMRouter.py:55`,
/// `:59`), so lnpnd and the interop helper look like stock lxmd to a peer
/// (order 1664). The store holds the whole sync limit, which a node's
/// announce is otherwise held to. A board names its own, `board_role_config`.
#[test]
fn announce_defaults_are_the_references_numbers() {
    let node = PropagationNode::new(
        MemoryPropagationStore::new(20_000_000),
        PropagationNodeConfig::default(),
    );
    let announce = PropagationNodeAnnounce::decode(&node.announce_app_data(1_700_000_000)).unwrap();
    assert!(!announce.legacy_support);
    assert!(announce.enabled);
    assert_eq!(announce.timebase, 1_700_000_000);
    assert_eq!(announce.transfer_limit_kb, 256);
    assert_eq!(announce.sync_limit_kb, 10_240);
    assert!(announce.metadata.is_empty());
}

/// Field 5 of a default node's own announce is the Lead's decision of
/// 2026-09-12, stamp cost 13 and peering cost 1, with flexibility 0 since
/// order 1294 (so it accepts 13 and up, what a stock peer takes). Peering
/// cost 0 would be wire-legal and dead: a stock peer's
/// `peering_key_ready` returns false on a falsy cost
/// (`reference/LXMF/LXMF/LXMPeer.py:228`) and its sync never pushes to us.
#[test]
fn the_default_config_announces_the_decided_costs() {
    let node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig::default(),
    );
    let announce = PropagationNodeAnnounce::decode(&node.announce_app_data(1_700_000_000)).unwrap();
    assert_eq!(
        [
            announce.stamp_cost,
            announce.stamp_cost_flexibility,
            announce.peering_cost
        ],
        [13, 0, 1]
    );
    assert_eq!(node.min_accepted_cost(), STOCK_PEER_MIN_STAMP_VALUE);
    assert!(!node.accepts_below_stock_peers());
}

/// A cost a stock peer would refuse to take from us is honoured but
/// flagged, so the host can say so at start (order 1294).
#[test]
fn a_floor_below_the_stock_peers_is_flagged() {
    let node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            stamp_cost_flexibility: 3,
            ..PropagationNodeConfig::default()
        },
    );
    assert_eq!(node.min_accepted_cost(), 10);
    assert!(node.accepts_below_stock_peers());
}

/// The board's deferred-durability path: a store flush that failed after
/// the accept must be able to un-remember the id, so the client's retry is
/// stored rather than answered as a proven duplicate.
#[test]
fn a_forgotten_id_is_accepted_again_as_new() {
    let mut node = node(64 * 1024);
    let bytes = envelope(7, 1);
    let transient_id = accepted_id(&node.handle_upload(&bytes, 100, stamp_of_value_zero));

    // Simulate the failed flush: the body never became durable.
    assert!(node.store_mut().purge(&transient_id).unwrap());
    node.forget_processed(&transient_id);

    match node.handle_upload(&bytes, 101, stamp_of_value_zero) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(!duplicate),
        other => panic!("expected acceptance, got {other:?}"),
    }
}

/// The clockless-bring-up epoch guard (#384 part 3): a record stored on the
/// birth anchor survives the first real time seed instead of reading as
/// 30 days old in one jump; records stamped after the jump still expire
/// normally.
///
/// The boundary is the one the seeding site reports (`note_calendar_jump`,
/// Codeberg #247), not a fixed date: the birth anchor is the build
/// timestamp, so both epochs look equally plausible by value.
#[test]
fn a_time_seed_does_not_mass_expire_uptime_era_records() {
    let floor = leviculum_core::constants::BUILD_UNIX_SECS;
    let mut seeded = node(64 * 1024);
    // Stored while the calendar was still birth-anchored: the build floor
    // plus a few seconds of uptime.
    let early =
        accepted_id(&seeded.handle_upload(&envelope(7, 1), floor + 300, stamp_of_value_zero));
    // The seed: the calendar jumps from the birth era to real time.
    seeded.note_calendar_jump(floor + 300);
    // Stored after the seed, long enough ago to be genuinely expired.
    let old =
        accepted_id(&seeded.handle_upload(&envelope(7, 2), floor + 365 * DAY, stamp_of_value_zero));

    let now = floor + 365 * DAY + 40 * DAY;
    let evicted = seeded.tick(now);
    assert_eq!(evicted.len(), 1, "only the post-jump record expires");
    assert_eq!(evicted[0].transient_id, old);
    assert!(seeded.store().contains(&early).unwrap());

    // Within one clockless boot, expiry still works: same epoch on both
    // sides of the comparison, and no jump reported.
    let mut clockless = node(64 * 1024);
    let stored = accepted_id(&clockless.handle_upload(&envelope(7, 3), floor, stamp_of_value_zero));
    let evicted = clockless.tick(floor + 31 * DAY);
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].transient_id, stored);
}

/// The board's resource SDU: Reticulum's smallest negotiated MTU (500)
/// less the resource per-part overhead — the most parts, so the most
/// per-part bookkeeping, of any link the board can hold.
const BOARD_RESOURCE_SDU: usize = 500 - leviculum_core::resource::RESOURCE_SDU_OVERHEAD;

/// The serve that killed a T114 on 2026-09-22, rebuilt byte for byte:
/// 24 stored messages, 224 B of body each, fetched in one round under the
/// board's announced 8 KB sync limit.
///
/// It reproduces the ARITHMETIC of the panic, not the panic: the board
/// died in `alloc`, and only a board with a 96 KiB heap can do that. What
/// this pins is the input to that death — that all 24 pass the cap, that
/// the encoded response is 5 427 B, and that the first framed copy of it
/// is 5 446 B, the exact size the board's `PANIC_PMRT` line names.
#[test]
fn the_field_fetch_frames_the_allocation_that_killed_the_board() {
    let mut node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            sync_limit_kb: 8,
            // The announced limit bounded the board's fetch that day; a
            // fetch is bounded by a serve limit now, so it stands in.
            serve_limit_bytes: Some(8000),
            ..cost_free()
        },
    );
    let wants: Vec<TransientId> = (0..24)
        .map(|seed| {
            accepted_id(&node.handle_upload(&envelope_sized(7, seed, 224), 0, stamp_of_value_zero))
        })
        .collect();

    let fetch = MessageGetRequest {
        wants: Some(wants.clone()),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1000)),
    };
    let outcome = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    let GetOutcome::Fetch { plan, .. } = outcome else {
        panic!("a fetch request must produce a fetch outcome");
    };
    let response = node.encode_fetch(&plan).unwrap();

    // The cap did not bite: the board's own PN_GET line said count=24
    // bytes=5376, and so does this.
    assert_eq!(plan.count(), 24, "all 24 must pass the 8 KB cap");
    assert_eq!(plan.served_bytes(), 5376);
    assert_eq!(response.len(), 5427, "the encoded MessageGetResponse");
    assert_eq!(
        plan.encoded_len(),
        response.len(),
        "the plan must predict the encoded length before it is built"
    );
    assert_eq!(
        response.len() + RESPONSE_FRAME_BYTES,
        5446,
        "the framed copy send_response builds before it checks the MDU -- \
         the exact byte count in the board's PANIC_PMRT line"
    );
}

/// The cap bounds the wire response, which is the premise
/// [`serve_peak_bytes`] rests on. Checked on the shape that maximises the
/// wire overhead per accounted byte: the smallest bodies the store takes,
/// as many as the cap allows.
#[test]
fn the_encoded_response_never_outgrows_the_accounted_cap() {
    for serve_limit_kb in [1usize, 2, 4, 8, 32] {
        let mut node = PropagationNode::new(
            MemoryPropagationStore::new(256 * 1024),
            PropagationNodeConfig {
                serve_limit_bytes: Some(serve_limit_kb * 1000),
                ..cost_free()
            },
        );
        let smallest = crate::constants::LXMF_OVERHEAD + 1;
        let wants: Vec<TransientId> = (0..u8::MAX)
            .map(|seed| {
                accepted_id(&node.handle_upload(
                    &envelope_sized(7, seed, smallest),
                    0,
                    stamp_of_value_zero,
                ))
            })
            .collect();
        let fetch = MessageGetRequest {
            wants: Some(wants),
            haves: None,
            transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
        };
        let GetOutcome::Fetch { plan, .. } = node
            .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
            .unwrap()
        else {
            panic!("a fetch request must produce a fetch outcome");
        };
        let response = node.encode_fetch(&plan).unwrap();
        assert_eq!(plan.encoded_len(), response.len());
        let cap = serve_limit_kb * 1000;
        assert!(
            response.len() <= cap,
            "serve_limit_kb={serve_limit_kb}: encoded {} > cap {cap}",
            response.len()
        );
    }
}

/// What the serve path costs the heap, at the cap the boards announce.
///
/// The number is asserted rather than described because it is the term
/// the board's role budget has to carry. Three revisions of it:
///
/// * **97 036 B** before #384 B1 — twelve times the cap, with four whole
///   response copies made for nothing (`packed`, `combined`,
///   `data_to_encrypt`, two hash scratch buffers).
/// * **48 922 B** after B1 — six times the cap, and half the board's
///   entire 96 KiB heap before any other allocation.
/// * **17 264 B** after B2, which is what this asserts: the response is
///   streamed out of the store into the resource parts, so the only
///   whole copy left is the transfer itself, and the cost is a bit over
///   twice the cap rather than six times it.
///
/// The old model is kept exact beside the new one
/// ([`serve_buffered_peak_bytes`]) so the board's own logs from before
/// 2026-09-23 stay readable against it.
#[test]
fn a_streamed_eight_kilobyte_serve_cap_costs_twice_its_size_not_six_times() {
    let peak = serve_peak_bytes(8_000, BOARD_RESOURCE_SDU);
    assert_eq!(peak, 17_264);
    assert!(
        peak < 3 * 8_000,
        "a streamed serve must cost less than three times the cap that bounds it"
    );

    // The path this replaced, still computable, so the before and the
    // after are one comparison rather than two commits apart.
    let buffered = serve_buffered_peak_bytes(8_000, BOARD_RESOURCE_SDU);
    assert_eq!(buffered, 48_922);
    assert!(
        peak * 2 < buffered,
        "B2 must more than halve the transient again: {peak} against {buffered}"
    );
}

/// The inverse the heap plan actually asks: given this much free heap,
/// how big a cap can be served? Monotone and tight — one byte more than
/// the answer does not fit.
#[test]
fn the_funded_cap_is_the_inverse_of_the_peak() {
    for budget in [0usize, 880, 5_000, 20_000, 17_264, 48_922, 200_000] {
        let cap = serve_cap_for_peak(budget, BOARD_RESOURCE_SDU);
        assert!(
            serve_peak_bytes(cap, BOARD_RESOURCE_SDU) <= budget || cap == 0,
            "budget {budget}: cap {cap} does not fit"
        );
        assert!(
            serve_peak_bytes(cap + 1, BOARD_RESOURCE_SDU) > budget,
            "budget {budget}: cap {cap} is not the largest that fits"
        );
    }
}

/// The answer to "what cap can today's plan honour": the T114's heap
/// budget leaves 784 B unclaimed, and under the streamed serve 784 B of
/// transient buys a cap of 216 B — the smallest LXMF message the store
/// takes, and nothing like a field one.
///
/// It was 88 B before #384 B2 and 104 B with B1's copies removed. The
/// point of the number is unchanged: the BOOT plan funds nothing useful
/// on this board, and what makes the board serve is reading the live
/// heap instead ([`serve_cap_for_live_heap`],
/// `leviculum-std/tests/mvr/pn_serve_cap_bounds_one_fetch.rs`).
///
/// (`budget_slack`, `leviculum-nrf/src/heap_census.rs` -- 784 B is
/// HEAP_SIZE 98 304 less the T114's node box 31 008, role 21 120,
/// reserve 18 848, four BLE sessions at 3 948 and four links at 2 664.
/// The board printed exactly that on 2026-09-23, six seconds before it
/// died serving a fetch.)
#[test]
fn todays_slack_funds_no_field_message() {
    let cap = serve_cap_for_peak(784, BOARD_RESOURCE_SDU);
    assert_eq!(
        cap, 216,
        "the T114's own boot slack must fund the cap its boot line prints"
    );
    // A field message is 224 B of body and a 32 B stamp, accounted with
    // 24 B up front and 16 B of per-message overhead.
    assert!(
        cap < 24 + 224 + STAMP_SIZE + 16,
        "784 B of slack must not be read as affording a field-sized serve (got {cap})"
    );
    // It is exactly the inverse of the peak, one byte either side.
    assert!(serve_peak_bytes(cap, BOARD_RESOURCE_SDU) <= 784);
    assert!(serve_peak_bytes(cap + 1, BOARD_RESOURCE_SDU) > 784);
}

/// The bound the funded cap buys: a fetch past it is answered with a
/// subset, and the rest is still there to be fetched next round.
///
/// This is the whole difference between the board of 2026-09-23 and the
/// board after it: the client asked for 24 messages, the node held 24,
/// and what it could not afford to send it also did not die of.
#[test]
fn a_funded_cap_serves_a_subset_and_keeps_the_rest() {
    // 18 of the field's 24 messages is what 30 380 B of heap funds
    // (`serve_cap_for_peak(30_380)` = 4 954 B); this pins the same
    // mechanic at a cap small enough to read.
    let cap = 24 + 3 * (160 + STAMP_SIZE + 16);
    let mut node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            sync_limit_kb: 8,
            serve_cap_bytes: Some(cap),
            ..cost_free()
        },
    );
    let wants: Vec<TransientId> = (0..8u8)
        .map(|seed| {
            accepted_id(&node.handle_upload(&envelope_sized(7, seed, 160), 0, stamp_of_value_zero))
        })
        .collect();

    let fetch = MessageGetRequest {
        wants: Some(wants.clone()),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
    };
    let GetOutcome::Fetch { plan, purged } = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    let served = plan.served_ids();

    assert_eq!(served.len(), 3, "the cap funds three of the eight");
    assert!(plan.encoded_len() <= cap, "the response must fit the cap");
    assert!(purged.is_empty(), "a fetch purges nothing by itself");
    assert_eq!(node.store().len(), 8, "the five unserved are still stored");

    // The unserved are the ones the next list offers, and a second fetch
    // takes the next three: bounded serving, not refusal.
    let rest: Vec<TransientId> = wants
        .iter()
        .copied()
        .filter(|id| !served.contains(id))
        .collect();
    let second = MessageGetRequest {
        wants: Some(rest),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
    };
    let GetOutcome::Fetch { plan, .. } = node
        .handle_get(&second.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    let served_two = plan.served_ids();
    assert_eq!(served_two.len(), 3, "the next round serves the next three");
    assert!(
        served_two.iter().all(|id| !served.contains(id)),
        "no message is served twice"
    );
}

/// **The streamed fetch IS the encoded fetch.** The bytes a board reads
/// out of its store, record by record, as the resource parts are cut are
/// the same bytes `MessageGetResponse::encode` would have produced —
/// which is what makes #384 B2 a heap change and not a wire change.
///
/// Read sizes are varied because the resource builder reads through a
/// scratch buffer sized from the link's SDU, and the source must not care:
/// a source whose framing depended on the read size would serve different
/// bytes on a BLE link than on LoRa.
#[test]
fn a_streamed_fetch_is_the_encoded_fetch() {
    use leviculum_core::resource::ResourceSource;

    // Body sizes chosen to straddle msgpack's bin8/bin16 boundary (255),
    // so the streamed `bin` header has to pick the same form the encoder
    // does, and array counts either side of fixarray's 16.
    for (count, body) in [
        (1usize, 120usize),
        (5, 300),
        (16, 113),
        (20, 255),
        (20, 256),
    ] {
        let mut node = PropagationNode::new(
            MemoryPropagationStore::new(256 * 1024),
            PropagationNodeConfig {
                sync_limit_kb: 64,
                ..cost_free()
            },
        );
        let wants: Vec<TransientId> = (0..count as u8)
            .map(|seed| {
                accepted_id(&node.handle_upload(
                    &envelope_sized(7, seed, body),
                    0,
                    stamp_of_value_zero,
                ))
            })
            .collect();
        let fetch = MessageGetRequest {
            wants: Some(wants),
            haves: None,
            transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
        };
        let GetOutcome::Fetch { plan, .. } = node
            .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
            .unwrap()
        else {
            panic!("a fetch request must produce a fetch outcome");
        };
        assert_eq!(plan.count(), count, "the whole mailbox must fit the cap");

        let encoded = node.encode_fetch(&plan).unwrap();
        assert_eq!(
            plan.encoded_len(),
            encoded.len(),
            "the plan's length must be the encoded length ({count} x {body} B)"
        );

        for chunk in [1usize, 7, 64, 464, 4096] {
            let mut source = node.fetch_source(&plan);
            assert_eq!(source.total_len(), encoded.len());
            let mut streamed = Vec::new();
            let mut buf = alloc::vec![0u8; chunk];
            loop {
                let read = source.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                streamed.extend_from_slice(&buf[..read]);
            }
            assert_eq!(
                streamed, encoded,
                "streamed fetch differs from the encoded one \
                 ({count} x {body} B, reads of {chunk})"
            );
            // And it is repeatable: the resource builder reads it twice.
            source.rewind().unwrap();
            let mut second = Vec::new();
            loop {
                let read = source.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                second.extend_from_slice(&buf[..read]);
            }
            assert_eq!(second, encoded, "the second pass must repeat the first");
        }
    }
}

/// A record that disappears between the plan and the stream fails the
/// build instead of shortening the response.
///
/// It cannot happen through the role's own verbs — the plan and the
/// stream run inside one synchronous serve, and deletion only happens in
/// `handle_get` — but the store is a trait and a board's log reclaims
/// pages on its own schedule. A short read would ship parts that do not
/// hash to the advertisement, which the receiver only discovers after
/// paying for every one of them.
#[test]
fn a_record_purged_under_a_plan_fails_the_stream() {
    use leviculum_core::resource::{ResourceSource, SourceError};

    let mut node = node(64 * 1024);
    let wants: Vec<TransientId> = (0..3u8)
        .map(|seed| accepted_id(&node.handle_upload(&envelope(7, seed), 0, stamp_of_value_zero)))
        .collect();
    let fetch = MessageGetRequest {
        wants: Some(wants.clone()),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
    };
    let GetOutcome::Fetch { plan, .. } = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(plan.count(), 3);

    node.store_mut().purge(&wants[1]).unwrap();

    let mut source = node.fetch_source(&plan);
    let mut buf = alloc::vec![0u8; 64];
    let error = loop {
        match source.read(&mut buf) {
            Ok(0) => panic!("a stream over a purged record must not reach the end"),
            Ok(_) => continue,
            Err(error) => break error,
        }
    };
    assert_eq!(error, SourceError::Unavailable);
}

/// The board's honest number today is 88 B, which serves nothing. What
/// that must NOT be is a refusal, a lost message, or a purge: the node
/// answers with an empty list of messages and keeps every one of them.
#[test]
fn a_cap_below_one_message_serves_nothing_and_loses_nothing() {
    let mut node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            sync_limit_kb: 8,
            serve_cap_bytes: Some(88),
            ..cost_free()
        },
    );
    let wants: Vec<TransientId> = (0..4u8)
        .map(|seed| accepted_id(&node.handle_upload(&envelope(7, seed), 0, stamp_of_value_zero)))
        .collect();

    let fetch = MessageGetRequest {
        wants: Some(wants),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
    };
    let GetOutcome::Fetch { plan, purged } = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    let response = node.encode_fetch(&plan).unwrap();
    assert!(plan.is_empty(), "88 B funds no message");
    assert_eq!(plan.served_bytes(), 0);
    assert!(purged.is_empty());
    assert_eq!(node.store().len(), 4, "nothing served is nothing lost");
    // A well-formed, decodable empty answer -- not an error, not nil.
    assert!(
        matches!(
            MessageGetResponse::decode(&response),
            Ok(MessageGetResponse::Messages(ref m)) if m.is_empty()
        ),
        "the short answer must still be a valid MessageGetResponse"
    );
}

/// Ten 3.7 kB messages for one mailbox, fetched by a client whose own
/// limit is 256 kB, from a node announcing `sync_limit_kb`. The shape of
/// the periculum cell `lxmf_pn_sync_limit_python_offers` (leviculum#388).
fn fetch_ten_with(config: PropagationNodeConfig) -> usize {
    let mut node = PropagationNode::new(MemoryPropagationStore::new(256 * 1024), config);
    let wants: Vec<TransientId> = (0..10u8)
        .map(|seed| {
            accepted_id(&node.handle_upload(&envelope_sized(7, seed, 3700), 0, stamp_of_value_zero))
        })
        .collect();
    let fetch = MessageGetRequest {
        wants: Some(wants),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(256)),
    };
    let GetOutcome::Fetch { plan, .. } = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    plan.count()
}

/// A host bounds a fetch by the client's limit alone, as the reference
/// does (`reference/LXMF/LXMF/LXMRouter.py:1526-1530`): its announced
/// per-sync limit paces what peers offer it, not what a client may take.
#[test]
fn a_host_bounds_a_fetch_by_the_clients_limit_alone() {
    let served = fetch_ten_with(PropagationNodeConfig {
        sync_limit_kb: 5,
        transfer_limit_kb: 4,
        ..cost_free()
    });
    assert_eq!(served, 10, "a 256 kB client limit takes all ten at once");
}

/// A board's serve limit still bounds a fetch: that bound is its heap
/// arithmetic, not its announce.
#[test]
fn a_boards_serve_limit_still_bounds_a_fetch() {
    let served = fetch_ten_with(PropagationNodeConfig {
        sync_limit_kb: 5,
        transfer_limit_kb: 4,
        serve_limit_bytes: Some(5000),
        ..cost_free()
    });
    assert_eq!(served, 1, "a 5000 B serve limit fits one 3.7 kB message");
}

/// leviculum#442 D6: a host at stamp cost 0 stores the TRUE value of what
/// it accepts, even before any peer demands one, as the reference does
/// (`validate_pn_stamp` expands the workblock at target cost 0 too,
/// `reference/LXMF/LXMF/LXStamper.py:95`). Stored as 0, the message would
/// be skipped forever by every stock peer that later joins
/// (`reference/LXMF/LXMF/LXMPeer.py:340`, min 16 - 3 here).
#[test]
fn a_host_at_cost_zero_offers_what_it_took_before_its_first_demanding_peer() {
    let mut node = PropagationNode::new(MemoryPropagationStore::new(64 * 1024), cost_free());
    // No peer in the table: the engine says no peer demands values.
    node.set_compute_stamp_value(false);
    assert!(
        node.compute_stamp_value(),
        "a host computes stamp values from the start"
    );

    let outcome = node.handle_upload(&envelope(7, 1), 100, |_, _| Some(20));
    let transient_id = accepted_id(&outcome);
    match outcome {
        UploadOutcome::Accepted { stamp_value, .. } => assert_eq!(stamp_value, 20),
        other => panic!("{other:?}"),
    }

    // Now a stock peer at min cost 13 is admitted.
    let announce = crate::propagation::PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 1,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 16,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        metadata: vec![],
    };
    let mut table = crate::peering::PeerTable::new(crate::peering::PeeringConfig::default());
    let _ = table.handle_announce([1; 16], &announce, Some(2), 200, false);
    let peer = table.get(&[1; 16]).expect("the announce peers the node");
    assert_eq!(peer.min_accepted_cost(), 13);

    let mut entries = Vec::new();
    node.store()
        .for_each(&mut |entry| entries.push(*entry))
        .unwrap();
    let plan = crate::peering::build_offer(peer, &entries, crate::peering::OFFER_BYTES_LIMIT)
        .expect("the stored message is offerable");
    assert_eq!(plan.ids, vec![transient_id]);
    assert_eq!(plan.skipped_low_value, 0);
}

/// The board keeps the saving (leviculum#442 D6): at cost 0 with no
/// demanding peer it skips the workblock and stores value 0.
#[test]
fn a_board_at_cost_zero_skips_the_stamp_value_until_a_peer_demands_one() {
    let mut node = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            always_compute_stamp_value: false,
            ..cost_free()
        },
    );
    assert!(!node.compute_stamp_value());
    let outcome = node.handle_upload(&envelope(7, 1), 100, no_validation);
    match outcome {
        UploadOutcome::Accepted { stamp_value, .. } => assert_eq!(stamp_value, 0),
        other => panic!("{other:?}"),
    }
    node.set_compute_stamp_value(true);
    let outcome = node.handle_upload(&envelope(7, 2), 100, |_, _| Some(20));
    match outcome {
        UploadOutcome::Accepted { stamp_value, .. } => assert_eq!(stamp_value, 20),
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The split `/get` (part 5 of #384): plan, fetch bodies, finish.
// ---------------------------------------------------------------------------

/// A store with a card's block cost and a body source the test controls.
///
/// The directory answers every metadata verb — so `prepare_get` can plan —
/// while `read_body` serves from a map the test can withdraw from. That
/// split is the whole point of the phase boundary: a message can still be
/// listed and located after its bytes are gone (a reclaimed segment, an
/// evicted record). The asynchronous prefetch skips it before finalizing
/// its response; a body disappearing after streaming starts is an error.
/// `space_required` charges whole 512-byte blocks with descriptor and commit.
struct FakeBodySource {
    entries: alloc::collections::BTreeMap<TransientId, StoredMessage>,
    bodies: alloc::collections::BTreeMap<TransientId, Vec<u8>>,
    /// Every `read_body` call, in order: the fetch phase's own record.
    reads: Rc<RefCell<Vec<TransientId>>>,
    capacity: u64,
    used: u64,
    next_sequence: u64,
    fail_purge: Option<TransientId>,
}

/// Bytes a block-backed store charges a body: `512 × (descriptor + payload
/// blocks + commit)`.
fn block_charge(body_len: usize) -> u64 {
    512 * (2 + (body_len as u64).div_ceil(512))
}

impl FakeBodySource {
    fn new(capacity: u64) -> (Self, Rc<RefCell<Vec<TransientId>>>) {
        let reads = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                entries: alloc::collections::BTreeMap::new(),
                bodies: alloc::collections::BTreeMap::new(),
                reads: reads.clone(),
                capacity,
                used: 0,
                next_sequence: 0,
                fail_purge: None,
            },
            reads,
        )
    }

    fn seed(&mut self, transient_id: TransientId, received_at: u64, body: &[u8]) {
        self.append(&transient_id, received_at, 0, body).unwrap();
    }

    /// The bytes are gone, the metadata is not.
    fn withdraw(&mut self, transient_id: &TransientId) {
        assert!(self.bodies.remove(transient_id).is_some());
        assert!(self.entries.contains_key(transient_id));
    }

    fn next_sequence(&mut self) -> u64 {
        self.next_sequence += 1;
        self.next_sequence
    }
}

impl PropagationStore for FakeBodySource {
    fn append(
        &mut self,
        transient_id: &TransientId,
        received_at: u64,
        stamp_value: u8,
        body: &[u8],
    ) -> Result<(), StorageError> {
        assert!(body.len() >= MIN_BODY_LEN);
        let replaced = self
            .entries
            .get(transient_id)
            .map_or(0, |meta| block_charge(meta.size as usize));
        let next = self.used - replaced + block_charge(body.len());
        if next > self.capacity {
            return Err(StorageError::Full);
        }
        let mut destination_hash = [0u8; DESTINATION_LENGTH];
        destination_hash.copy_from_slice(&body[..DESTINATION_LENGTH]);
        let sequence = self.next_sequence();
        self.entries.insert(
            *transient_id,
            StoredMessage {
                transient_id: *transient_id,
                destination_hash,
                size: body.len() as u32,
                received_at,
                stamp_value,
                sequence,
            },
        );
        self.bodies.insert(*transient_id, body.to_vec());
        self.used = next;
        Ok(())
    }

    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        for meta in self.entries.values() {
            visit(meta);
        }
        Ok(())
    }

    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
        if self.fail_purge.as_ref() == Some(transient_id) {
            return Err(StorageError::Corrupt);
        }
        let Some(meta) = self.entries.remove(transient_id) else {
            return Ok(false);
        };
        self.used -= block_charge(meta.size as usize);
        self.bodies.remove(transient_id);
        Ok(true)
    }

    fn free_space(&self) -> u64 {
        self.capacity - self.used
    }

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn space_required(&self, body_len: usize) -> u64 {
        block_charge(body_len)
    }

    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
        Ok(self.entries.contains_key(transient_id))
    }
}

impl PropagationBodyStore for FakeBodySource {
    fn read_body(&self, transient_id: &TransientId) -> Result<Option<Vec<u8>>, StorageError> {
        self.reads.borrow_mut().push(*transient_id);
        Ok(self.bodies.get(transient_id).cloned())
    }

    fn read_body_at(
        &self,
        transient_id: &TransientId,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<Option<usize>, StorageError> {
        Ok(self
            .bodies
            .get(transient_id)
            .map(|body| crate::propagation_store::copy_from(body, offset, buf)))
    }
}

/// The caller reads only the bounded plan, in wire order, retaining its
/// readable bodies until the streamed resource has been built.
fn fetch_bodies(
    node: &PropagationNode<FakeBodySource>,
    prepared: &PreparedFetch,
) -> Vec<(TransientId, Vec<u8>)> {
    let mut bodies = Vec::new();
    for (transient_id, _) in prepared.plan.messages() {
        if let Some(body) = node.store().read_body(transient_id).unwrap() {
            bodies.push((*transient_id, body));
        }
    }
    bodies
}

/// The body `envelope_sized(destination, seed, lxmf_len)` stores, with the
/// transient ID that upload computes — so a test can re-upload it.
fn seeded(destination: u8, seed: u8, lxmf_len: usize) -> (TransientId, Vec<u8>) {
    let mut unstamped = vec![destination; DESTINATION_LENGTH];
    unstamped.resize(lxmf_len, seed);
    let transient_id = full_hash(&unstamped);
    unstamped.extend_from_slice(&[0xEE; STAMP_SIZE]);
    (transient_id, unstamped)
}

const SMALL: usize = 928;
const OVER: usize = 929;

#[test]
fn a_split_fetch_reads_only_the_bounded_plan_and_the_wrapper_matches_it() {
    // Two identical stores: one driven through the split, one through the
    // synchronous wrapper. Both hold a body exactly at the transfer limit
    // (24 + 960 + 16 = 1000), one one byte over it, and one whose bytes are
    // gone.
    let (fits_id, fits_body) = seeded(7, 1, SMALL);
    let (over_id, over_body) = seeded(7, 2, OVER);
    let (gone_id, gone_body) = seeded(7, 3, 300);
    let (stranger_id, stranger_body) = seeded(9, 4, 120);
    let stranger = [0x99; 32];
    assert_ne!(stranger, stranger_id);

    let build = || {
        let (mut source, reads) = FakeBodySource::new(64 * 1024);
        source.seed(fits_id, 10, &fits_body);
        source.seed(over_id, 20, &over_body);
        source.seed(gone_id, 30, &gone_body);
        source.seed(stranger_id, 40, &stranger_body);
        source.withdraw(&gone_id);
        (PropagationNode::new(source, cost_free()), reads)
    };
    let (mut split, reads) = build();

    // Oversized and foreign IDs are removed before a card read, and the
    // exact-fit body exhausts the 1 kB budget. Repeating it cannot cause a
    // second read or a second message in the response.
    let fetch = MessageGetRequest {
        wants: Some(vec![
            over_id,
            fits_id,
            fits_id,
            gone_id,
            stranger_id,
            stranger,
        ]),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1)),
    };
    let request = fetch.encode().unwrap();
    let PreparedGet::Fetch(plan) = split
        .prepare_get(&request, &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch form must plan a fetch");
    };
    assert_eq!(plan.plan.served_ids(), vec![fits_id]);
    assert!(plan.purged.is_empty());
    assert_eq!(plan.plan.served_bytes(), SMALL as u64);
    assert!(reads.borrow().is_empty(), "planning reads no body");

    let bodies = fetch_bodies(&split, &plan);
    assert_eq!(
        *reads.borrow(),
        vec![fits_id],
        "only the planned body is read, once"
    );
    assert_eq!(bodies.len(), 1);

    let split_outcome = split.finish_get(plan, &bodies).unwrap();
    match &split_outcome {
        GetOutcome::Fetch { plan, purged } => {
            assert_eq!(
                plan.served_ids(),
                vec![fits_id],
                "at the limit, not over it"
            );
            assert_eq!(plan.served_bytes(), SMALL as u64);
            assert!(purged.is_empty());
        }
        other => panic!("{other:?}"),
    }

    // The synchronous wrapper returns the same plan without body reads.
    let (mut one_shot, wrapper_reads) = build();
    let wrapper_outcome = one_shot
        .handle_get(&request, &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    assert_eq!(split_outcome, wrapper_outcome);
    assert!(wrapper_reads.borrow().is_empty(), "handle_get only plans");

    // And the list form needs no fetch at all: `prepare_get` answers it.
    let list = MessageGetRequest::list().encode().unwrap();
    let prepared = split
        .prepare_get(&list, &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    let PreparedGet::Ready(list_outcome) = prepared else {
        panic!("the list form has no body to fetch");
    };
    assert_eq!(
        list_outcome,
        one_shot
            .handle_get(&list, &[7; DESTINATION_LENGTH], 0)
            .unwrap()
    );
    match list_outcome {
        GetOutcome::List { count, .. } => assert_eq!(count, 3),
        other => panic!("{other:?}"),
    }
}

/// What a synced message the peer got wrong leaves: a stamp below the cost,
/// a body too short to hold one, and a nested multi-message transfer all
/// count against the sender, so its batch throttles it once drained.
#[test]
fn a_bad_stamp_a_bad_body_and_a_nested_transfer_count_against_the_sender() {
    let mut demanding = PropagationNode::new(
        MemoryPropagationStore::new(64 * 1024),
        PropagationNodeConfig {
            stamp_cost: 13,
            ..PropagationNodeConfig::default()
        },
    );
    let invalid = demanding.accept_stamped(&envelope_body(7, 1), 100, |_, _| None);
    assert!(matches!(invalid, UploadOutcome::InvalidStamp { .. }));
    assert!(invalid.counts_against_sender());

    let malformed = node(64 * 1024).accept_stamped(&[7; 40], 100, stamp_of_value_zero);
    assert!(matches!(malformed, UploadOutcome::Malformed(_)));
    assert!(malformed.counts_against_sender());

    assert!(UploadOutcome::PeerSyncForm.counts_against_sender());
}

/// A store that could not hold a synced message is our failure, not the
/// peer's: it does not count against the sender.
#[test]
fn a_store_failure_does_not_count_against_the_sender() {
    assert!(!UploadOutcome::StoreFailed(StorageError::Full).counts_against_sender());
    assert!(!UploadOutcome::StoreFailed(StorageError::Io).counts_against_sender());
}

/// A stored message, new or already held, does not count against the
/// sender.
#[test]
fn an_accepted_message_does_not_count_against_the_sender() {
    let mut node = node(64 * 1024);
    let first = node.accept_stamped(&envelope_body(7, 1), 100, stamp_of_value_zero);
    assert!(matches!(
        first,
        UploadOutcome::Accepted {
            duplicate: false,
            ..
        }
    ));
    assert!(!first.counts_against_sender());
    let again = node.accept_stamped(&envelope_body(7, 1), 101, stamp_of_value_zero);
    assert!(matches!(
        again,
        UploadOutcome::Accepted {
            duplicate: true,
            ..
        }
    ));
    assert!(!again.counts_against_sender());
}

/// One stamped message body (`lxmf_data ‖ stamp`), the form a peer sync
/// carries, for `destination`, unique per `seed`.
fn envelope_body(destination: u8, seed: u8) -> Vec<u8> {
    let mut body = vec![destination; DESTINATION_LENGTH];
    body.resize(120, seed);
    body.extend_from_slice(&[0xEE; STAMP_SIZE]);
    body
}

fn link_identity(seed: u8) -> leviculum_core::Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    leviculum_core::Identity::from_private_key_bytes(&private).expect("valid identity")
}

/// The destination the stack itself builds for `identity` and these
/// aspects: the independent derivation the link gates are checked against.
fn destination_of(identity: &leviculum_core::Identity, aspect: &str) -> [u8; DESTINATION_LENGTH] {
    *leviculum_core::Destination::new(
        Some(identity.clone()),
        leviculum_core::Direction::In,
        leviculum_core::DestinationType::Single,
        "lxmf",
        &[aspect],
    )
    .expect("valid destination")
    .hash()
    .as_bytes()
}

/// A `/get` over a link that never identified is refused with
/// `ERROR_NO_IDENTITY` before the request is read.
#[test]
fn a_get_over_an_unidentified_link_is_answered_no_identity() {
    assert_eq!(get_mailbox(None), Err(PeerError::NoIdentity));
}

/// An identified link drains the identity's own `lxmf.delivery` mailbox,
/// and a second identity drains a different one.
#[test]
fn a_get_over_an_identified_link_drains_that_identitys_delivery_mailbox() {
    let identity = link_identity(1);
    assert_eq!(
        get_mailbox(Some(&identity)),
        Ok(destination_of(&identity, "delivery"))
    );
    assert_ne!(
        get_mailbox(Some(&link_identity(2))),
        get_mailbox(Some(&identity))
    );
}

/// An `/offer` over a link that never identified is refused with
/// `ERROR_NO_IDENTITY` before any gate runs.
#[test]
fn an_offer_over_an_unidentified_link_is_answered_no_identity() {
    assert_eq!(offer_remote(None), Err(PeerError::NoIdentity));
}

/// An identified offerer is known by its identity hash and its
/// `lxmf.propagation` destination.
#[test]
fn an_offer_over_an_identified_link_names_the_identity_and_its_propagation_destination() {
    let identity = link_identity(3);
    assert_eq!(
        offer_remote(Some(&identity)),
        Ok(OfferRemote {
            identity_hash: *identity.hash(),
            propagation_hash: destination_of(&identity, "propagation"),
        })
    );
}

/// Audit 2026-10-12 D15: an inbound propagation link is let go strictly
/// above 180 s without payload (`reference/LXMF/LXMF/LXMRouter.py:970`),
/// read on the whole-second clock `lnpnd` and the board both feed it. A
/// link not yet established has no data clock and is never idle.
#[test]
fn an_idle_inbound_link_is_let_go_strictly_above_the_reference_bound() {
    use crate::constants::PROPAGATION_LINK_MAX_INACTIVITY_SECS as BOUND;
    assert!(!link_idle_past_bound(None));
    assert!(!link_idle_past_bound(Some(0)));
    assert!(
        !link_idle_past_bound(Some(BOUND * 1000)),
        "at the bound it stays"
    );
    assert!(
        !link_idle_past_bound(Some(BOUND * 1000 + 999)),
        "a fraction of a second past is still the bound's second"
    );
    assert!(link_idle_past_bound(Some((BOUND + 1) * 1000)));
    assert!(
        link_idle_past_bound(Some(725_000)),
        "the core's stale horizon"
    );
}

#[test]
fn a_split_fetch_purges_before_it_fetches_and_keeps_the_duplicate_guard() {
    let (keep_id, keep_body) = seeded(7, 1, 300);
    let (ack_id, ack_body) = seeded(7, 2, 300);

    let build = || {
        let (mut source, reads) = FakeBodySource::new(64 * 1024);
        source.seed(keep_id, 10, &keep_body);
        source.seed(ack_id, 20, &ack_body);
        (
            PropagationNode::new(
                source,
                PropagationNodeConfig {
                    // The SD board skips stamp values at cost 0 while no
                    // peer demands them; no_validation asserts that policy.
                    always_compute_stamp_value: false,
                    ..cost_free()
                },
            ),
            reads,
        )
    };
    let (mut node, reads) = build();

    let request = MessageGetRequest {
        wants: Some(vec![keep_id, ack_id]),
        haves: Some(vec![ack_id]),
        transfer_limit_kb: Some(TransferLimit::Integer(1)),
    }
    .encode()
    .unwrap();
    let PreparedGet::Fetch(plan) = node
        .prepare_get(&request, &[7; DESTINATION_LENGTH], 500)
        .unwrap()
    else {
        panic!("a fetch form must plan a fetch");
    };

    // The purge is done and named before a single body is read: the caller
    // can flush it to a deferred store before any response byte exists.
    assert_eq!(plan.purged, vec![ack_id]);
    assert!(!node.store().contains(&ack_id).unwrap());
    assert_eq!(
        plan.plan.served_ids(),
        vec![keep_id],
        "the purged id is not served"
    );
    assert!(reads.borrow().is_empty(), "no body read before the purge");

    let bodies = fetch_bodies(&node, &plan);
    let outcome = node.finish_get(plan, &bodies).unwrap();
    match &outcome {
        GetOutcome::Fetch { plan, purged } => {
            assert_eq!(plan.served_ids(), vec![keep_id]);
            assert_eq!(*purged, vec![ack_id]);
        }
        other => panic!("{other:?}"),
    }

    // The confirmation is remembered, so the client's retry of what it just
    // took is a duplicate rather than a re-acceptance.
    match node.handle_upload(&envelope_sized(7, 2, 300), 600, no_validation) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(duplicate),
        other => panic!("{other:?}"),
    }

    // The wrapper agrees with the split path on the same input, purge and
    // all.
    let (mut one_shot, _) = build();
    assert_eq!(
        outcome,
        one_shot
            .handle_get(&request, &[7; DESTINATION_LENGTH], 500)
            .unwrap()
    );
}

/// Repeated wants cannot pin a body copy per ID occurrence: the metadata
/// wrapper applies the transfer limit and deduplicates the plan before
/// packet encoding or a card prefetch can read any bodies.
#[test]
fn the_wrapper_plans_no_more_bodies_than_the_budget_can_carry() {
    use leviculum_core::resource::ResourceSource;

    // A 4,032-byte body under a 1 kB limit: metadata excludes it before
    // any read, including every repeat of the same oversized ID.
    let (huge_id, huge_body) = seeded(7, 1, 4_000);
    let (mut source, reads) = FakeBodySource::new(64 * 1024);
    source.seed(huge_id, 10, &huge_body);
    let mut node = PropagationNode::new(source, PropagationNodeConfig::default());

    let request = MessageGetRequest {
        wants: Some(vec![huge_id; 32]),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1)),
    }
    .encode()
    .unwrap();
    let outcome = node
        .handle_get(&request, &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    match &outcome {
        GetOutcome::Fetch { plan, purged } => {
            assert!(plan.is_empty(), "nothing fits the 1 kB budget");
            assert_eq!(plan.served_bytes(), 0);
            assert!(purged.is_empty());
            let response = node.encode_fetch(plan).unwrap();
            assert_eq!(response.len(), plan.encoded_len());
            assert!(response.len() <= 1_000, "the reply stays within budget");
            assert!(matches!(
                MessageGetResponse::decode(&response),
                Ok(MessageGetResponse::Messages(bodies)) if bodies.is_empty()
            ));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        reads.borrow().is_empty(),
        "an oversized ID and its repeats are never read"
    );

    // Under 2 kB, two distinct 960-byte stored bodies fit
    // (24 + 2 * (960 + 16) = 1,976). Repeating the first ID serves it
    // once, and the third distinct body is withheld when the remaining
    // budget cannot carry even the smallest stored body.
    let (fits_id, fits_body) = seeded(7, 2, SMALL);
    let (second_id, second_body) = seeded(7, 3, SMALL);
    let (third_id, third_body) = seeded(7, 4, SMALL);
    let (mut source, reads) = FakeBodySource::new(64 * 1024);
    source.seed(fits_id, 20, &fits_body);
    source.seed(second_id, 30, &second_body);
    source.seed(third_id, 40, &third_body);
    let mut node = PropagationNode::new(source, PropagationNodeConfig::default());
    let mut wants = vec![fits_id; 32];
    wants.extend([second_id, third_id]);

    let request = MessageGetRequest {
        wants: Some(wants),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(2)),
    }
    .encode()
    .unwrap();
    let outcome = node
        .handle_get(&request, &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    assert!(reads.borrow().is_empty(), "the wrapper only reads metadata");
    match &outcome {
        GetOutcome::Fetch { plan, purged } => {
            assert_eq!(plan.served_ids(), vec![fits_id, second_id]);
            assert_eq!(plan.count(), 2);
            assert_eq!(plan.served_bytes(), 2 * SMALL as u64);
            assert!(purged.is_empty());
            let response = node.encode_fetch(plan).unwrap();
            assert_eq!(response.len(), plan.encoded_len());
            assert!(response.len() <= 2_000, "the reply stays within budget");
            assert_eq!(
                MessageGetResponse::decode(&response).unwrap(),
                MessageGetResponse::Messages(vec![
                    fits_body[..SMALL].to_vec(),
                    second_body[..SMALL].to_vec(),
                ])
            );
            let mut source = node.fetch_source(plan);
            let mut streamed = Vec::with_capacity(plan.encoded_len());
            let mut buf = [0u8; 64];
            loop {
                let copied = source.read(&mut buf).unwrap();
                if copied == 0 {
                    break;
                }
                streamed.extend_from_slice(&buf[..copied]);
            }
            assert_eq!(streamed, response);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        *reads.borrow(),
        vec![fits_id, second_id],
        "encoding reads each planned body once; repeats and the withheld ID are not read"
    );
}

/// Eviction must free what the store charges the incoming body, not its raw
/// length: a block-backed store frees blocks, so a raw-byte target stops
/// short and the retried append fails.
#[test]
fn eviction_frees_the_stores_block_cost_not_the_raw_body() {
    // Capacity 5120: two 332-byte bodies charge 1536 each, leaving 2048.
    let (mut source, _) = FakeBodySource::new(5120);
    let (first_id, first_body) = seeded(7, 1, 300);
    let (second_id, second_body) = seeded(7, 2, 300);
    source.seed(first_id, 10, &first_body);
    source.seed(second_id, 20, &second_body);
    assert_eq!(source.free_space(), 2048);
    let mut node = PropagationNode::new(source, cost_free());

    // A 1532-byte body charges 2560 and does not fit. Its raw length (1532)
    // is *under* the free space, so an eviction target measured in raw bytes
    // would free nothing, the retried append would report Full, and the
    // upload this node owed a proof for would be dropped.
    let outcome = node.handle_upload(&envelope_sized(7, 3, 1500), 30, stamp_of_value_zero);
    match &outcome {
        UploadOutcome::Accepted {
            evicted, duplicate, ..
        } => {
            assert!(!duplicate);
            assert_eq!(evicted.len(), 1, "one record freed 1536, enough for 2560");
            assert_eq!(evicted[0].size, 332);
            assert_eq!(evicted[0].reason, EvictionReason::Displaced);
        }
        other => panic!("a fittable-after-eviction body must be accepted: {other:?}"),
    }
    assert!(node.store().contains(&accepted_id(&outcome)).unwrap());
    assert_eq!(node.store().free_space(), 5120 - 1536 - 2560);
}

/// The asynchronous owner can discover a record was reclaimed after the
/// directory snapshot. It is skipped before resource lengths are fixed,
/// without re-filling the funded snapshot from unplanned IDs.
#[test]
fn a_split_fetch_skips_unavailable_bodies_before_streaming() {
    let (gone_id, gone_body) = seeded(7, 1, 300);
    let (keep_id, keep_body) = seeded(7, 2, 300);
    let (unplanned_id, unplanned_body) = seeded(7, 3, 300);
    let (mut store, reads) = FakeBodySource::new(64 * 1024);
    store.seed(gone_id, 10, &gone_body);
    store.seed(keep_id, 20, &keep_body);
    store.seed(unplanned_id, 30, &unplanned_body);
    let mut node = PropagationNode::new(store, cost_free());
    let request = MessageGetRequest {
        wants: Some(vec![gone_id, keep_id, unplanned_id]),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1)),
    }
    .encode()
    .unwrap();
    let PreparedGet::Fetch(prepared) = node
        .prepare_get(&request, &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch form must plan a fetch");
    };
    assert_eq!(prepared.plan.served_ids(), vec![gone_id, keep_id]);
    assert!(reads.borrow().is_empty());
    node.store_mut().withdraw(&gone_id);
    let bodies = fetch_bodies(&node, &prepared);
    assert_eq!(*reads.borrow(), vec![gone_id, keep_id]);
    assert_eq!(bodies.len(), 1);
    let GetOutcome::Fetch { plan, purged } = node.finish_get(prepared, &bodies).unwrap() else {
        panic!("a fetch must finish with a fetch outcome");
    };
    assert_eq!(plan.served_ids(), vec![keep_id]);
    assert_eq!(plan.served_bytes(), 300);
    assert!(purged.is_empty());
    let expected = MessageGetResponse::Messages(vec![keep_body[..300].to_vec()])
        .encode()
        .unwrap();
    assert_eq!(plan.encoded_len(), expected.len());
    let mut source = FetchSource::new(bodies.as_slice(), &plan);
    for _ in 0..2 {
        source.rewind().unwrap();
        let mut streamed = Vec::with_capacity(plan.encoded_len());
        let mut buf = [0u8; 7];
        loop {
            let copied = source.read(&mut buf).unwrap();
            if copied == 0 {
                break;
            }
            streamed.extend_from_slice(&buf[..copied]);
        }
        assert_eq!(streamed, expected);
    }
}

/// Prefetching cannot turn a record of a different size into permission to
/// exceed the snapshot's serve budget, or advertise a truncated response.
#[test]
fn a_split_fetch_rejects_changed_body_lengths() {
    let (id, body) = seeded(7, 1, SMALL);
    let (mut store, reads) = FakeBodySource::new(64 * 1024);
    store.seed(id, 10, &body);
    let mut node = PropagationNode::new(store, cost_free());
    let request = MessageGetRequest {
        wants: Some(vec![id]),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(1)),
    }
    .encode()
    .unwrap();
    let PreparedGet::Fetch(prepared) = node
        .prepare_get(&request, &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch form must plan a fetch");
    };
    assert!(reads.borrow().is_empty());
    for len in [body.len() - 1, body.len() + 1] {
        let mut changed = body.clone();
        changed.resize(len, 0xEE);
        let bodies = vec![(id, changed)];
        assert!(matches!(
            node.finish_get(prepared.clone(), &bodies),
            Err(GetError::Store(StorageError::Corrupt))
        ));
        let mut source = FetchSource::new(bodies.as_slice(), &prepared.plan);
        let mut buf = [0u8; 64];
        let error = loop {
            match source.read(&mut buf) {
                Ok(0) => panic!("a changed body must not reach the stream's end"),
                Ok(_) => {}
                Err(error) => break error,
            }
        };
        assert_eq!(error, SourceError::LengthChanged);
    }
}

#[test]
fn committed_confirmation_survives_a_later_purge_failure() {
    let (first, first_body) = seeded(7, 1, 300);
    let (second, second_body) = seeded(7, 2, 300);
    let (mut store, _) = FakeBodySource::new(64 * 1024);
    store.seed(first, 10, &first_body);
    store.seed(second, 20, &second_body);
    store.fail_purge = Some(second);
    let mut node = PropagationNode::new(
        store,
        PropagationNodeConfig {
            always_compute_stamp_value: false,
            ..cost_free()
        },
    );
    let request = MessageGetRequest {
        wants: Some(vec![]),
        haves: Some(vec![first, second]),
        transfer_limit_kb: None,
    }
    .encode()
    .unwrap();
    assert!(matches!(
        node.prepare_get(&request, &[7; DESTINATION_LENGTH], 500),
        Err(GetError::Store(StorageError::Corrupt))
    ));
    assert!(!node.store().contains(&first).unwrap());
    assert!(node.store().contains(&second).unwrap());
    match node.handle_upload(&envelope_sized(7, 1, 300), 600, no_validation) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(duplicate),
        other => panic!("{other:?}"),
    }
    assert!(!node.store().contains(&first).unwrap());
}

#[test]
fn funded_list_returns_the_smallest_owned_ids_within_the_wire_ceiling() {
    let (small, small_body) = seeded(7, 1, 80);
    let (middle, middle_body) = seeded(7, 2, 100);
    let (large, large_body) = seeded(7, 3, 120);
    let (other, other_body) = seeded(8, 4, 64);
    let (mut store, _) = FakeBodySource::new(64 * 1024);
    for (id, body) in [
        (large, &large_body),
        (other, &other_body),
        (small, &small_body),
        (middle, &middle_body),
    ] {
        store.seed(id, 10, body);
    }
    let mut node = PropagationNode::new(
        store,
        PropagationNodeConfig {
            serve_limit_bytes: Some(71),
            serve_cap_bytes: Some(200),
            ..cost_free()
        },
    );
    let request = MessageGetRequest::list().encode().unwrap();
    let PreparedGet::Ready(GetOutcome::List { response, count }) = node
        .prepare_get(&request, &[7; DESTINATION_LENGTH], 500)
        .unwrap()
    else {
        panic!("the list request must return a list");
    };
    assert_eq!(count, 2);
    assert!(response.len() <= 71);
    assert_eq!(
        MessageListResponse::decode(&response).unwrap(),
        MessageListResponse::TransientIds(vec![small, middle])
    );
    assert!(node.store().contains(&large).unwrap());
}

#[test]
fn list_pressure_reports_throttling_instead_of_hiding_stored_mail() {
    let (id, body) = seeded(7, 1, 80);
    let (mut store, _) = FakeBodySource::new(64 * 1024);
    store.seed(id, 10, &body);
    let mut node = PropagationNode::new(
        store,
        PropagationNodeConfig {
            serve_cap_bytes: Some(34),
            ..cost_free()
        },
    );
    let request = MessageGetRequest::list().encode().unwrap();
    let PreparedGet::Ready(GetOutcome::List { response, .. }) = node
        .prepare_get(&request, &[7; DESTINATION_LENGTH], 500)
        .unwrap()
    else {
        panic!("the list request must return its wire response");
    };
    assert_eq!(
        MessageListResponse::decode(&response).unwrap(),
        MessageListResponse::Error(PeerError::Throttled)
    );
    assert!(node.store().contains(&id).unwrap());
}

/// Orders 1295 and 1370, leviculum#478: the processed-ID cache forgets an
/// entry once its bucket (a week) is past the 180-day window, not before,
/// and keeps the younger ones.
#[test]
fn the_processed_cache_is_pruned_a_bucket_after_its_oldest_is_due() {
    const DAY: u64 = 24 * 60 * 60;
    let mut node = node(64 * 1024);
    let old = envelope(7, 1);
    let young = envelope(7, 2);
    let start = 1_000;
    let _ = accepted_id(&node.handle_upload(&old, start, stamp_of_value_zero));
    let _ = accepted_id(&node.handle_upload(&young, start + 100 * DAY, stamp_of_value_zero));
    let is_duplicate =
        |node: &mut PropagationNode<MemoryPropagationStore>, bytes: &[u8], now| match node
            .handle_upload(bytes, now, stamp_of_value_zero)
        {
            UploadOutcome::Accepted { duplicate, .. } => duplicate,
            other => panic!("{other:?}"),
        };

    // Due by the window, inside the slack: the store forgot both, the
    // cache still knows both.
    let due = start + PROCESSED_ID_EXPIRY_SECS + 10;
    assert_eq!(
        node.tick(due).len(),
        2,
        "both messages expired from the store"
    );
    assert!(is_duplicate(&mut node, &old, due));
    assert!(is_duplicate(&mut node, &young, due));

    // Past the slack: the old one is forgotten and stored again as new,
    // the young one is still a duplicate.
    let pruned = due + PROCESSED_ID_PRUNE_SLACK_SECS;
    assert!(node.tick(pruned).is_empty());
    assert!(!is_duplicate(&mut node, &old, pruned));
    assert!(is_duplicate(&mut node, &young, pruned));
}

/// The memory store, its whole-store walks counted.
struct WalkCounted {
    store: MemoryPropagationStore,
    walks: core::cell::Cell<usize>,
}

impl PropagationStore for WalkCounted {
    fn append(
        &mut self,
        transient_id: &TransientId,
        received_at: u64,
        stamp_value: u8,
        body: &[u8],
    ) -> Result<(), StorageError> {
        self.store
            .append(transient_id, received_at, stamp_value, body)
    }
    fn for_each(&self, visit: &mut dyn FnMut(&StoredMessage)) -> Result<(), StorageError> {
        self.walks.set(self.walks.get() + 1);
        self.store.for_each(visit)
    }
    fn purge(&mut self, transient_id: &TransientId) -> Result<bool, StorageError> {
        self.store.purge(transient_id)
    }
    fn free_space(&self) -> u64 {
        self.store.free_space()
    }
    fn capacity(&self) -> u64 {
        self.store.capacity()
    }
    fn contains(&self, transient_id: &TransientId) -> Result<bool, StorageError> {
        self.store.contains(transient_id)
    }
}

fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// A memory store of `capacity` filled until an append fails, bodies of 120
/// to 520 bytes for one of eight destinations, received over the 30 days
/// before `now`.
fn full_store(capacity: u64, now: u64, state: &mut u64) -> WalkCounted {
    let mut store = MemoryPropagationStore::new(capacity);
    loop {
        let mut transient_id = [0u8; 32];
        for chunk in transient_id.chunks_mut(8) {
            chunk.copy_from_slice(&next_random(state).to_le_bytes());
        }
        let len = 120 + (next_random(state) % 401) as usize;
        let body = vec![(next_random(state) % 8) as u8; len];
        let received_at = now - next_random(state) % (30 * DAY);
        if store.append(&transient_id, received_at, 0, &body).is_err() {
            break;
        }
    }
    WalkCounted {
        store,
        walks: core::cell::Cell::new(0),
    }
}

/// The eviction order before order 1370: every stored message weighed and
/// stably sorted, heaviest first (`get_weight`,
/// `reference/LXMF/LXMF/LXMRouter.py:1056-1067`, sorted at `:1194`).
fn weight_order(
    store: &MemoryPropagationStore,
    prioritised: &[[u8; DESTINATION_LENGTH]],
    now: u64,
) -> Vec<TransientId> {
    let mut weighted: Vec<(f64, TransientId)> = Vec::new();
    store
        .for_each(&mut |meta| {
            let age = now.saturating_sub(meta.received_at);
            let age_weight = (age as f64 / (4.0 * DAY as f64)).max(1.0);
            let priority = if prioritised.contains(&meta.destination_hash) {
                0.1
            } else {
                1.0
            };
            weighted.push((priority * age_weight * meta.size as f64, meta.transient_id));
        })
        .unwrap();
    weighted.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap());
    weighted.into_iter().map(|(_, id)| id).collect()
}

/// Order 1370: a full append evicts exactly what the whole-store sort it
/// replaced evicted, the heaviest first until the body fits, over random
/// stores with sizes, ages and a prioritised destination; and the uploads
/// after it, inside the reference's store-job cadence, keep taking the
/// sorted order's next messages.
#[test]
fn eviction_takes_the_reference_weight_order_from_one_walk() {
    let now = 1_790_000_000;
    let mut state = 0x9e37_79b9_7f4a_7c15;
    for case in 0..40 {
        let store = full_store(20_000 + next_random(&mut state) % 40_000, now, &mut state);
        let prioritised = vec![[(next_random(&mut state) % 8) as u8; DESTINATION_LENGTH]];
        let order = weight_order(&store.store, &prioritised, now);
        let config = PropagationNodeConfig {
            prioritised_destinations: prioritised,
            ..cost_free()
        };
        let mut node = PropagationNode::new(store, config);
        let mut evicted: Vec<TransientId> = Vec::new();
        for upload in 0..6u8 {
            let bytes = envelope_sized(9, upload, 200 + 60 * upload as usize);
            match node.handle_upload(&bytes, now + upload as u64, stamp_of_value_zero) {
                UploadOutcome::Accepted {
                    duplicate: false,
                    evicted: displaced,
                    ..
                } => evicted.extend(displaced.iter().map(|eviction| eviction.transient_id)),
                other => panic!("case {case}, upload {upload}: {other:?}"),
            }
        }
        assert!(
            !evicted.is_empty(),
            "case {case}: a full store evicted nothing"
        );
        assert_eq!(
            evicted,
            order[..evicted.len()],
            "case {case}: not the reference's weight order"
        );
    }
}

/// Order 1370: uploads into a full store walk it once per
/// [`EVICTION_CANDIDATE_FRACTION`] of its capacity, not once per upload, at
/// 4 MB and 16 MB alike, and again once the candidates are
/// [`EVICTION_CANDIDATES_SECS`] old. The candidate list holds what one
/// fraction is worth, not the store. The positive control is the walk
/// count of the sort it replaced: one per upload that found the store
/// full.
#[test]
fn a_full_store_is_walked_once_per_candidate_fraction_not_per_upload() {
    let now = 1_790_000_000;
    let mut state = 0x2545_f491_4f6c_dd1d;
    for capacity in [4 << 20u64, 16 << 20] {
        let mut node = PropagationNode::new(full_store(capacity, now, &mut state), cost_free());
        let messages = node.store().store.len();
        let uploads = 200usize;
        let (mut displacing, mut most_candidates) = (0usize, 0usize);
        for upload in 0..uploads {
            let bytes = envelope_sized((upload / 256) as u8 + 9, upload as u8, 300);
            match node.handle_upload(&bytes, now + 1, stamp_of_value_zero) {
                UploadOutcome::Accepted {
                    duplicate: false,
                    evicted,
                    ..
                } => displacing += usize::from(!evicted.is_empty()),
                other => panic!("{capacity}: upload {upload}: {other:?}"),
            }
            most_candidates = most_candidates.max(node.eviction_candidates.len());
        }
        let walks = node.store().walks.get();
        // 200 uploads of about 330 stored bytes against candidates worth
        // capacity / 1024 plus one upload each walk.
        let fraction = capacity / EVICTION_CANDIDATE_FRACTION;
        let bound = (uploads as u64 * 340).div_ceil(fraction + 300) + 1;
        std::eprintln!(
            "MAKE_ROOM_PIN capacity={capacity} messages={messages} uploads={uploads} \
             displacing={displacing} walks={walks} bound={bound} most_candidates={most_candidates}"
        );
        assert!(
            displacing > uploads / 2,
            "{capacity}: the store was not full"
        );
        assert!(
            walks as u64 <= bound,
            "{capacity}: {walks} walks exceed {bound}"
        );
        assert!(
            walks * 4 < displacing,
            "{capacity}: {walks} walks for {displacing} full uploads, the sort's count"
        );
        assert!(
            (most_candidates as u64) * 120 <= fraction + 2 * 520,
            "{capacity}: {most_candidates} candidates for {fraction} bytes"
        );

        // Past the cadence the first full upload weighs afresh, whatever
        // candidates are left.
        let before = node.store().walks.get();
        let late = now + 1 + EVICTION_CANDIDATES_SECS;
        let displaced = (0..50u8).find(|seed| {
            let bytes = envelope_sized(200, *seed, 300);
            matches!(
                node.handle_upload(&bytes, late, stamp_of_value_zero),
                UploadOutcome::Accepted { evicted, .. } if !evicted.is_empty()
            )
        });
        assert!(
            displaced.is_some(),
            "{capacity}: the store stopped being full"
        );
        assert_eq!(
            node.store().walks.get(),
            before + 1,
            "{capacity}: stale candidates reused"
        );
    }
}

/// Order 1394: each path out of the store records its own reason, and
/// `/lev/status` answers it. Collected by a `haves` confirmation,
/// displaced by a full store, expired by the tick, delivered to the own
/// mailbox; a stored one is held, a never-seen one unknown.
#[test]
fn every_path_out_of_the_store_records_its_reason() {
    use crate::lev_status::{DepartureReason, MessageStatus};

    let mut node = node(340);
    node.serve_lev_status();
    let collected = accepted_id(&node.handle_upload(&envelope(7, 1), 0, stamp_of_value_zero));
    assert_eq!(node.status_of(&collected), MessageStatus::Held);
    let acknowledge = MessageGetRequest::acknowledge(vec![collected]);
    node.handle_get(
        &acknowledge.encode().unwrap(),
        &[7; DESTINATION_LENGTH],
        DAY,
    )
    .unwrap();

    let displaced = accepted_id(&node.handle_upload(&envelope(7, 2), DAY, stamp_of_value_zero));
    let kept = accepted_id(&node.handle_upload(&envelope(7, 3), 9 * DAY, stamp_of_value_zero));
    let newest = accepted_id(&node.handle_upload(&envelope(7, 4), 10 * DAY, stamp_of_value_zero));
    assert!(!node.store().contains(&displaced).unwrap());

    assert!(node.store_mut().purge(&kept).unwrap());
    node.note_delivered_locally(&kept, 11 * DAY);

    let expired = node.tick(10 * DAY + MESSAGE_EXPIRY_SECS + 1);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].transient_id, newest);

    let fresh = node.take_fresh_departures();
    assert_eq!(
        fresh,
        vec![
            (collected, DepartureReason::Collected),
            (displaced, DepartureReason::Evicted),
            (kept, DepartureReason::Delivered),
            (newest, DepartureReason::Expired),
        ]
    );
    assert!(node.take_fresh_departures().is_empty(), "taken once");

    // The tick also dropped what left more than the window ago: the
    // collected and the displaced one left at day 1, before the window.
    assert_eq!(node.status_of(&collected), MessageStatus::Unknown);
    assert_eq!(node.status_of(&displaced), MessageStatus::Unknown);
    assert_eq!(node.status_of(&kept), MessageStatus::Delivered);
    assert_eq!(node.status_of(&newest), MessageStatus::Expired);
    assert_eq!(node.status_of(&[0x55; 32]), MessageStatus::Unknown);
}

/// Inside the window every reason answers as itself.
#[test]
fn the_status_answers_each_reason_inside_the_window() {
    use crate::lev_status::{decode_answer, MessageStatus};

    let mut node = node(340);
    node.serve_lev_status();
    let collected = accepted_id(&node.handle_upload(&envelope(7, 1), 0, stamp_of_value_zero));
    let acknowledge = MessageGetRequest::acknowledge(vec![collected]);
    node.handle_get(&acknowledge.encode().unwrap(), &[7; DESTINATION_LENGTH], 1)
        .unwrap();
    let displaced = accepted_id(&node.handle_upload(&envelope(7, 2), 2, stamp_of_value_zero));
    let _ = node.handle_upload(&envelope(7, 3), 9 * DAY, stamp_of_value_zero);
    let held = accepted_id(&node.handle_upload(&envelope(7, 4), 10 * DAY, stamp_of_value_zero));
    assert!(!node.store().contains(&displaced).unwrap());

    let answer = node.answer_lev_status(&[collected, displaced, held, [0x55; 32]]);
    assert_eq!(
        decode_answer(&answer).unwrap().unwrap(),
        vec![
            (collected, MessageStatus::Collected),
            (displaced, MessageStatus::Evicted),
            (held, MessageStatus::Held),
            ([0x55; 32], MessageStatus::Unknown),
        ]
    );
}

/// An expired id answers `expired` after a restart too, but the restore
/// does not make it a duplicate: the file held no expired ids before
/// order 1394, and a peer that offers one past the window received it
/// after us, a new copy here too. (Before the restart the RAM duplicate
/// cache still holds it from the accept, unchanged by this order.)
#[test]
fn an_expired_departure_is_answered_but_not_restored_as_a_duplicate() {
    use crate::lev_status::{DepartureReason, MessageStatus};

    let mut node = node(64 * 1024);
    node.track_departures();
    let bytes = envelope(7, 1);
    let id = accepted_id(&node.handle_upload(&bytes, 0, stamp_of_value_zero));
    let now = MESSAGE_EXPIRY_SECS + 1;
    assert_eq!(node.tick(now).len(), 1);
    assert_eq!(node.status_of(&id), MessageStatus::Expired);
    assert_eq!(
        node.departures()
            .map(|(id, d)| (*id, *d))
            .collect::<Vec<_>>(),
        vec![(
            id,
            Departure {
                at_secs: now,
                reason: DepartureReason::Expired
            }
        )]
    );

    let mut restarted = self::node(64 * 1024);
    let departure = Departure {
        at_secs: now,
        reason: DepartureReason::Expired,
    };
    assert_eq!(restarted.restore_departures([(id, departure)], now + 1), 1);
    assert_eq!(restarted.status_of(&id), MessageStatus::Expired);
    match restarted.handle_upload(&bytes, now + 1, stamp_of_value_zero) {
        UploadOutcome::Accepted { duplicate, .. } => assert!(!duplicate),
        other => panic!("{other:?}"),
    }
}

/// The announce carries `{"lev": {"status": 1}}` under `PN_META_CUSTOM`
/// only on a node that serves the path: a board, which does not, announces
/// what it did before.
#[test]
fn only_a_node_that_serves_the_status_announces_it() {
    use crate::lev_status::{announced_version, PN_META_CUSTOM};

    let mut node = node(64 * 1024);
    let plain = PropagationNodeAnnounce::decode(&node.announce_app_data(1)).unwrap();
    assert!(plain.metadata.iter().all(|(key, _)| *key != PN_META_CUSTOM));
    assert_eq!(announced_version(&plain), None);

    node.track_departures();
    let tracking = PropagationNodeAnnounce::decode(&node.announce_app_data(1)).unwrap();
    assert_eq!(
        announced_version(&tracking),
        None,
        "tracking alone is not serving"
    );

    node.serve_lev_status();
    let serving = node.announce_app_data(1);
    let announce = PropagationNodeAnnounce::decode(&serving).unwrap();
    assert_eq!(announced_version(&announce), Some(1));
    // The whole entry, key and value, is 16 bytes on the wire.
    let raw = &announce
        .metadata
        .iter()
        .find(|(key, _)| *key == PN_META_CUSTOM)
        .unwrap()
        .1;
    assert_eq!(
        raw.as_slice(),
        b"\x81\xa3lev\x81\xa6status\x01",
        "{{\"lev\": {{\"status\": 1}}}}"
    );
    assert_eq!(serving.len(), plain_len(&node) + 2 + raw.len());

    fn plain_len(node: &PropagationNode<MemoryPropagationStore>) -> usize {
        let mut announce = PropagationNodeAnnounce::decode(&node.announce_app_data(1)).unwrap();
        announce.metadata.retain(|(key, _)| *key != PN_META_CUSTOM);
        announce.encode().unwrap().len()
    }
}

/// Order 1558 (LV-1000): the duplicate list is bounded by count, not only
/// by time. On a near-empty store the floor holds: past it the oldest
/// processed ids and departures go, the newest stay duplicates and keep
/// their `/lev/status` answer, and neither half ever holds more than its
/// bound plus the departures' eighth of slack, the departures held by the
/// host's [`PropagationNode::bound_departures`].
#[test]
fn the_duplicate_list_is_held_to_its_count_bound() {
    use crate::lev_status::MessageStatus;

    let mut node = node(64 * 1024);
    node.track_departures();
    let (processed_cap, departed_cap) = dup_caps(0);
    assert_eq!(processed_cap, DUP_IDS_FLOOR);
    let id_of = |n: u32| {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(&n.to_be_bytes());
        id[4..8].copy_from_slice(&n.wrapping_mul(0x9e37_79b9).to_be_bytes());
        id
    };
    let total = (DUP_IDS_FLOOR + DUP_IDS_FLOOR / 4) as u32;
    for n in 0..total {
        node.note_delivered_locally(&id_of(n), 1_000 + u64::from(n));
        // As a host does each time it takes the departures.
        node.bound_departures();
        let counts = node.dup_counts();
        assert!(counts.processed <= processed_cap, "{counts:?}");
        assert!(
            counts.departed <= departed_cap + departed_cap / 8,
            "{counts:?}"
        );
    }
    let counts = node.dup_counts();
    assert_eq!(counts.processed_cap, processed_cap);
    assert_eq!(counts.departed_cap, departed_cap);
    assert!(
        counts.processed >= processed_cap - processed_cap / 8,
        "{counts:?}"
    );
    assert!(counts.departed >= departed_cap, "{counts:?}");
    let newest = id_of(total - 1);
    let now = 1_000 + u64::from(total);
    assert!(node.knows(&newest, now));
    assert_eq!(node.status_of(&newest), MessageStatus::Delivered);
    let oldest = id_of(0);
    assert!(
        !node.knows(&oldest, now),
        "the oldest id outlived the bound"
    );
    assert_eq!(node.status_of(&oldest), MessageStatus::Unknown);

    // A restore of more departures than the bound keeps the newest.
    let mut restarted = self::node(64 * 1024);
    let departures = (0..total).map(|n| {
        (
            id_of(n),
            Departure {
                at_secs: 1_000 + u64::from(n),
                reason: crate::lev_status::DepartureReason::Delivered,
            },
        )
    });
    restarted.restore_departures(departures, now);
    let counts = restarted.dup_counts();
    assert!(
        counts.departed <= departed_cap + departed_cap / 8,
        "{counts:?}"
    );
    assert!(counts.processed <= processed_cap, "{counts:?}");
    assert!(restarted.knows(&newest, now));
}

/// Order 1558: the bound follows the store's count as it is when the list
/// reaches it, not as a tick last read it. A store holding more than the
/// floor's worth gets its two ids a message without any tick.
#[test]
fn the_duplicate_bound_follows_the_store_without_a_tick() {
    let stored = DUP_IDS_FLOOR / 2 + 8_192;
    let mut store = MemoryPropagationStore::new(u64::MAX / 4);
    for n in 0..stored as u32 {
        let mut body = vec![0u8; MIN_BODY_LEN];
        body[..4].copy_from_slice(&n.to_be_bytes());
        let mut id = [0xeeu8; 32];
        id[..4].copy_from_slice(&n.to_be_bytes());
        store.append(&id, 1_000, 0, &body).unwrap();
    }
    // No departures tracked, so only the processed ids' own path can read
    // the count.
    let mut node = PropagationNode::new(store, cost_free());
    let (processed_cap, _) = dup_caps(stored);
    assert!(processed_cap > DUP_IDS_FLOOR);
    for n in 0..processed_cap as u32 {
        let mut id = [0x11u8; 32];
        id[..4].copy_from_slice(&n.to_be_bytes());
        node.note_delivered_locally(&id, 2_000 + u64::from(n));
    }
    let counts = node.dup_counts();
    assert_eq!(counts.processed_cap, processed_cap, "{counts:?}");
    assert_eq!(counts.departed_cap, 0, "{counts:?}");
    assert!(counts.processed > DUP_IDS_FLOOR, "{counts:?}");
    assert!(counts.processed <= processed_cap, "{counts:?}");
}

// ---- the heap refusal a board logs (order 1676, LV-1006) ----

/// A node over two stored 200 B messages for destination 7, serving to
/// `serve_cap` beside an 8 000 B serve limit (the board's shape).
fn heap_capped(serve_cap: Option<usize>) -> (PropagationNode<FakeBodySource>, [TransientId; 2]) {
    let (first, first_body) = seeded(7, 1, 200);
    let (second, second_body) = seeded(7, 2, 200);
    let (mut store, _) = FakeBodySource::new(64 * 1024);
    store.seed(first, 10, &first_body);
    store.seed(second, 10, &second_body);
    let node = PropagationNode::new(
        store,
        PropagationNodeConfig {
            serve_limit_bytes: Some(8_000),
            serve_cap_bytes: serve_cap,
            ..cost_free()
        },
    );
    (node, [first, second])
}

fn fetch_both(ids: [TransientId; 2], limit_kb: u64) -> Vec<u8> {
    MessageGetRequest {
        wants: Some(ids.to_vec()),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(limit_kb)),
    }
    .encode()
    .unwrap()
}

/// One 200 B message accounts to 24 + 232 + 16 = 272 B. A heap cap one
/// byte under that serves nothing although the client and the serve limit
/// would take both: the round is the heap's refusal, and the mail stays.
#[test]
fn a_heap_cap_under_one_message_is_a_heap_refusal_and_keeps_the_mail() {
    let (mut node, ids) = heap_capped(Some(271));
    let outcome = node
        .handle_get(&fetch_both(ids, 1000), &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    let GetOutcome::Fetch { plan, purged } = &outcome else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert!(plan.is_empty());
    assert!(purged.is_empty());
    assert_eq!(plan.heap_withheld(), 2);
    assert!(node.refused_for_heap(&outcome));
    assert_eq!(
        node.encode_fetch(plan).unwrap(),
        vec![0x90],
        "the wire answer is the empty list"
    );
    assert!(node.store().contains(&ids[0]).unwrap());
    assert!(node.store().contains(&ids[1]).unwrap());

    // The heap frees: the same request is served whole.
    node.set_serve_cap_bytes(Some(8_000));
    let outcome = node
        .handle_get(&fetch_both(ids, 1000), &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    let GetOutcome::Fetch { plan, .. } = &outcome else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(plan.served_ids(), ids.to_vec());
    assert_eq!(plan.heap_withheld(), 0);
    assert!(!node.refused_for_heap(&outcome));
}

/// A cap that funds one of the two serves it and withholds the other: a
/// short answer, which the protocol allows, not a refusal.
#[test]
fn a_heap_cap_that_funds_some_mail_is_a_short_answer_not_a_refusal() {
    let (mut node, ids) = heap_capped(Some(272));
    let outcome = node
        .handle_get(&fetch_both(ids, 1000), &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    let GetOutcome::Fetch { plan, .. } = &outcome else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(plan.count(), 1);
    assert_eq!(plan.heap_withheld(), 1);
    assert!(!node.refused_for_heap(&outcome));
}

/// The client's own limit is not the heap: a round its limit empties,
/// with or without a looser heap cap, withholds nothing for the heap.
#[test]
fn a_round_the_clients_limit_empties_is_no_heap_refusal() {
    for cap in [None, Some(8_000)] {
        let (mut node, ids) = heap_capped(cap);
        // 0 kB: the client takes nothing at all.
        let outcome = node
            .handle_get(&fetch_both(ids, 0), &[7; DESTINATION_LENGTH], 0)
            .unwrap();
        let GetOutcome::Fetch { plan, .. } = &outcome else {
            panic!("a fetch request must produce a fetch outcome");
        };
        assert!(plan.is_empty());
        assert_eq!(plan.heap_withheld(), 0, "cap {cap:?}");
        assert!(!node.refused_for_heap(&outcome), "cap {cap:?}");
    }
}

/// The list form: a heap cap that funds no id answers `ERROR_THROTTLED`
/// and is the heap's refusal; the same answer forced by the serve limit
/// is not, and a list the heap funds is not either.
#[test]
fn a_throttled_list_is_a_heap_refusal_only_when_the_heap_caused_it() {
    let list = MessageGetRequest::list().encode().unwrap();

    let (mut node, _) = heap_capped(Some(34));
    let outcome = node.handle_get(&list, &[7; DESTINATION_LENGTH], 0).unwrap();
    let GetOutcome::List { response, count } = &outcome else {
        panic!("the list request must return a list");
    };
    assert_eq!(*count, 0);
    assert_eq!(
        MessageListResponse::decode(response).unwrap(),
        MessageListResponse::Error(PeerError::Throttled)
    );
    assert!(node.refused_for_heap(&outcome));

    let (store, _) = FakeBodySource::new(64 * 1024);
    let mut limited = PropagationNode::new(
        store,
        PropagationNodeConfig {
            serve_limit_bytes: Some(34),
            serve_cap_bytes: Some(34),
            ..cost_free()
        },
    );
    let outcome = limited
        .handle_get(&list, &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    assert!(!limited.refused_for_heap(&outcome));

    let (mut funded, _) = heap_capped(Some(8_000));
    let outcome = funded
        .handle_get(&list, &[7; DESTINATION_LENGTH], 0)
        .unwrap();
    assert!(matches!(outcome, GetOutcome::List { count: 2, .. }));
    assert!(!funded.refused_for_heap(&outcome));
}
