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
    let early = accepted_id(&seeded.handle_upload(&envelope(7, 1), floor + 300, no_validation));
    // The seed: the calendar jumps from the birth era to real time.
    seeded.note_calendar_jump(floor + 300);
    // Stored after the seed, long enough ago to be genuinely expired.
    let old = accepted_id(&seeded.handle_upload(&envelope(7, 2), floor + 365 * DAY, no_validation));

    let now = floor + 365 * DAY + 40 * DAY;
    let evicted = seeded.tick(now);
    assert_eq!(evicted.len(), 1, "only the post-jump record expires");
    assert_eq!(evicted[0].transient_id, old);
    assert!(seeded.store().contains(&early).unwrap());

    // Within one clockless boot, expiry still works: same epoch on both
    // sides of the comparison, and no jump reported.
    let mut clockless = node(64 * 1024);
    let stored = accepted_id(&clockless.handle_upload(&envelope(7, 3), floor, no_validation));
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
            ..PropagationNodeConfig::default()
        },
    );
    let wants: Vec<TransientId> = (0..24)
        .map(|seed| {
            accepted_id(&node.handle_upload(&envelope_sized(7, seed, 224), 0, no_validation))
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
    let GetOutcome::Fetch {
        response,
        served,
        served_bytes,
        ..
    } = outcome
    else {
        panic!("a fetch request must produce a fetch outcome");
    };

    // The cap did not bite: the board's own PN_GET line said count=24
    // bytes=5376, and so does this.
    assert_eq!(served.len(), 24, "all 24 must pass the 8 KB cap");
    assert_eq!(served_bytes, 5376);
    assert_eq!(response.len(), 5427, "the encoded MessageGetResponse");
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
    for sync_limit_kb in [1u64, 2, 4, 8, 32] {
        let mut node = PropagationNode::new(
            MemoryPropagationStore::new(256 * 1024),
            PropagationNodeConfig {
                sync_limit_kb,
                ..PropagationNodeConfig::default()
            },
        );
        let smallest = crate::constants::LXMF_OVERHEAD + 1;
        let wants: Vec<TransientId> = (0..u8::MAX)
            .map(|seed| {
                accepted_id(&node.handle_upload(
                    &envelope_sized(7, seed, smallest),
                    0,
                    no_validation,
                ))
            })
            .collect();
        let fetch = MessageGetRequest {
            wants: Some(wants),
            haves: None,
            transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
        };
        let GetOutcome::Fetch { response, .. } = node
            .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
            .unwrap()
        else {
            panic!("a fetch request must produce a fetch outcome");
        };
        let cap = (sync_limit_kb * 1000) as usize;
        assert!(
            response.len() <= cap,
            "sync_limit_kb={sync_limit_kb}: encoded {} > cap {cap}",
            response.len()
        );
    }
}

/// What the serve path costs the heap, at the cap the boards announce.
///
/// The number is asserted rather than described because it is the term
/// the board's role budget has to carry: an 8 KB announced serve limit
/// buys a 48 922 B transient -- six times the cap, and half of the
/// board's entire 96 KiB heap, before a single other allocation.
///
/// It was 97 036 B, twelve times the cap, until #384 B1 removed the four
/// copies the path made for nothing (`packed`, `combined`,
/// `data_to_encrypt`, and the two hash scratch buffers). Half the term
/// is gone; the term is still larger than the heap can spare, which is
/// why the boot line still reports a deficit rather than a fit.
#[test]
fn an_eight_kilobyte_serve_cap_costs_six_times_its_size() {
    let peak = serve_peak_bytes(8_000, BOARD_RESOURCE_SDU);
    assert_eq!(peak, 48_922);
    assert!(
        peak > 6 * 8_000,
        "the serve transient is more than six times the cap that bounds it"
    );
    assert!(
        peak < 97_036,
        "B1 must not have grown the transient it was ordered to shrink"
    );
}

/// The inverse the heap plan actually asks: given this much free heap,
/// how big a cap can be served? Monotone and tight — one byte more than
/// the answer does not fit.
#[test]
fn the_funded_cap_is_the_inverse_of_the_peak() {
    for budget in [0usize, 880, 5_000, 20_000, 48_922, 200_000] {
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
/// budget leaves 880 B unclaimed, and 880 B of transient buys a serve cap
/// of 104 bytes -- less than one stored message. Halving the per-byte
/// cost (#384 B1) doubled the affordable cap and it is still nothing:
/// the plan as it stands funds no useful serve at all.
///
/// (`budget_slack`, `leviculum-nrf/src/heap_census.rs` -- 784 B is
/// HEAP_SIZE 98 304 less the T114's node box 31 008, role 21 120,
/// reserve 18 848, four BLE sessions at 3 948 and four links at 2 664.
/// The board printed exactly that on 2026-09-23, six seconds before it
/// died serving a fetch; a const probe of this tree's own
/// `budget_serve_cap` on the T114's `NodeCore` prints 784 and 88.)
#[test]
fn todays_slack_funds_no_useful_serve_cap() {
    let cap = serve_cap_for_peak(784, BOARD_RESOURCE_SDU);
    assert_eq!(
        cap, 88,
        "the T114's own boot slack must fund the cap its boot line prints"
    );
    assert!(
        cap < crate::constants::LXMF_OVERHEAD,
        "784 B of slack must not be read as affording a message-sized serve (got {cap})"
    );
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
            ..PropagationNodeConfig::default()
        },
    );
    let wants: Vec<TransientId> = (0..8u8)
        .map(|seed| {
            accepted_id(&node.handle_upload(&envelope_sized(7, seed, 160), 0, no_validation))
        })
        .collect();

    let fetch = MessageGetRequest {
        wants: Some(wants.clone()),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
    };
    let GetOutcome::Fetch {
        response,
        served,
        purged,
        ..
    } = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };

    assert_eq!(served.len(), 3, "the cap funds three of the eight");
    assert!(response.len() <= cap, "the response must fit the cap");
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
    let GetOutcome::Fetch {
        served: served_two, ..
    } = node
        .handle_get(&second.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert_eq!(served_two.len(), 3, "the next round serves the next three");
    assert!(
        served_two.iter().all(|id| !served.contains(id)),
        "no message is served twice"
    );
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
            ..PropagationNodeConfig::default()
        },
    );
    let wants: Vec<TransientId> = (0..4u8)
        .map(|seed| accepted_id(&node.handle_upload(&envelope(7, seed), 0, no_validation)))
        .collect();

    let fetch = MessageGetRequest {
        wants: Some(wants),
        haves: None,
        transfer_limit_kb: Some(TransferLimit::Integer(100_000)),
    };
    let GetOutcome::Fetch {
        response,
        served,
        served_bytes,
        purged,
    } = node
        .handle_get(&fetch.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("a fetch request must produce a fetch outcome");
    };
    assert!(served.is_empty(), "88 B funds no message");
    assert_eq!(served_bytes, 0);
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
    // And the list still offers all four, so the client knows they exist.
    let list = MessageGetRequest {
        wants: None,
        haves: None,
        transfer_limit_kb: None,
    };
    let GetOutcome::List { count, .. } = node
        .handle_get(&list.encode().unwrap(), &[7; DESTINATION_LENGTH], 0)
        .unwrap()
    else {
        panic!("an empty request is the list form");
    };
    assert_eq!(count, 4, "the node still offers what it cannot yet serve");
}
