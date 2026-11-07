//! Every msgpack shape the home node and its devices exchange, encoded and
//! decoded again over the whole range of each field (1173): the requests
//! and answers of `loquax::home::wire`, the archive entries a pull carries
//! (names, petnames, peer states, marks, imported rows of both versions),
//! the stored fields map and the admission code. Integers at `i64::MIN`,
//! `i64::MAX` and `u64::MAX`, empty strings, strings of 64 KiB, empty and
//! full vectors, every enum variant. 256 cases each from a fixed seed, so
//! a red case is the same on every host.
//!
//! Where a reader normalises, the property says how: a peer state's mute
//! at or below 0 reads as unmuted, an empty petname as none, and a mark
//! holds 1 to `MARK_IDS_MAX` ids, the rest refused.

use leviculum_lxmf::msgpack;
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;
use loquax::home::code::HomeCode;
use loquax::home::wire::{self, Entry, Mark, MarkFlag, Page, PeerState, Status, Submission};
use loquax::store::ImportRow;
use loquax::ui::Signature;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};

/// 256 cases from a fixed seed, no regression files written.
fn config() -> Config {
    Config {
        cases: 256,
        rng_seed: RngSeed::Fixed(0x1173),
        failure_persistence: None,
        ..Config::default()
    }
}

/// 64 KiB.
const LONG: usize = 64 * 1024;

/// Any text: empty, short of any script, or 64 KiB of one- and multi-byte
/// characters.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        any::<String>(),
        "[a-zA-Z0-9äöüß😀🐀 ]{0,64}",
        Just("x".repeat(LONG)),
        Just("ü".repeat(LONG / 2)),
        Just("😀".repeat(LONG / 4)),
    ]
}

/// Any bytes: empty, short, or 64 KiB.
fn bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(Vec::new()),
        prop::collection::vec(any::<u8>(), 0..64),
        Just(vec![0xff; LONG]),
    ]
}

fn u64s() -> impl Strategy<Value = u64> {
    prop_oneof![Just(0), Just(u64::MAX), Just(i64::MAX as u64), any::<u64>()]
}

fn i64s() -> impl Strategy<Value = i64> {
    prop_oneof![
        Just(0),
        Just(i64::MIN),
        Just(i64::MAX),
        Just(-1),
        any::<i64>()
    ]
}

fn status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Ok),
        Just(Status::NotIdentified),
        Just(Status::NotAdmitted),
        Just(Status::BadToken),
        Just(Status::Malformed),
        Just(Status::Failed),
    ]
}

fn entry() -> impl Strategy<Value = Entry> {
    (u64s(), any::<u8>(), i64s(), bytes()).prop_map(|(seq, kind, at, packed)| Entry {
        seq,
        kind,
        at,
        packed,
    })
}

/// A field as LXMF carries it: the key, and a msgpack value (a `bin`).
fn field() -> impl Strategy<Value = Field> {
    (i64s(), bytes()).prop_map(|(key, value)| {
        let mut packed = Vec::new();
        msgpack::bin(&mut packed, &value);
        (key, packed)
    })
}

fn fields() -> impl Strategy<Value = Vec<Field>> {
    prop_oneof![Just(Vec::new()), prop::collection::vec(field(), 1..6),]
}

fn delivery() -> impl Strategy<Value = Option<DeliveryState>> {
    prop_oneof![
        Just(None),
        Just(Some(DeliveryState::Queued)),
        Just(Some(DeliveryState::Stamping)),
        Just(Some(DeliveryState::InFlight)),
        Just(Some(DeliveryState::Sent)),
        Just(Some(DeliveryState::InMailbox)),
        Just(Some(DeliveryState::Delivered)),
        Just(Some(DeliveryState::Rejected)),
        Just(Some(DeliveryState::Failed)),
    ]
}

fn signature() -> impl Strategy<Value = Signature> {
    prop_oneof![
        Just(Signature::Unverified),
        Just(Signature::Verified),
        Just(Signature::Invalid),
    ]
}

/// A finite sender's clock: a NaN is never equal to itself, and no
/// source writes one.
fn timestamp() -> impl Strategy<Value = Option<f64>> {
    prop_oneof![
        Just(None),
        Just(Some(0.0)),
        Just(Some(f64::MAX)),
        Just(Some(f64::MIN)),
        Just(Some(-1.5)),
        prop::num::f64::NORMAL.prop_map(Some),
    ]
}

fn import_row() -> impl Strategy<Value = ImportRow> {
    (
        (any::<[u8; 32]>(), any::<bool>(), timestamp()),
        (prop::option::of(i64s()), bytes(), text(), fields()),
        (delivery(), signature(), prop::option::of(any::<bool>())),
        prop::option::of((any::<[u8; 16]>(), bytes())),
    )
        .prop_map(
            |(
                (id, outbound, timestamp),
                (received_at, title, content, fields),
                (delivery, signature, origin_verdict),
                held,
            )| ImportRow {
                id,
                outbound,
                timestamp,
                received_at,
                title,
                content,
                fields,
                delivery,
                signature,
                origin_verdict,
                held,
            },
        )
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn a_join_request_and_a_name_entry_round_trip(token in any::<[u8; 16]>(), name in text()) {
        prop_assert_eq!(
            wire::read_join_request(&wire::join_request(&token, &name)),
            Ok((token, name.clone()))
        );
        prop_assert_eq!(
            wire::read_name_entry(&wire::name_entry(&token, &name)),
            Ok((token, name))
        );
    }

    #[test]
    fn a_petname_entry_round_trips_an_empty_one_as_none(
        peer in any::<[u8; 16]>(),
        name in prop::option::of(text()),
    ) {
        let read = wire::read_petname_entry(&wire::petname_entry(&peer, name.as_deref()));
        prop_assert_eq!(read, Ok((peer, name.filter(|n| !n.is_empty()))));
    }

    #[test]
    fn a_status_round_trips(status in status()) {
        prop_assert_eq!(wire::read_status(&wire::status_answer(status)), Ok(status));
    }

    #[test]
    fn a_cursor_and_a_wake_round_trip(cursor in u64s()) {
        prop_assert_eq!(wire::read_sync_request(&wire::sync_request(cursor)), Ok(cursor));
        prop_assert_eq!(wire::read_watch_request(&wire::watch_request(cursor)), Ok(cursor));
        prop_assert_eq!(wire::read_watch_answer(&wire::watch_answer(cursor)), Ok(Ok(cursor)));
        prop_assert_eq!(wire::read_wake(&wire::wake(cursor)), Ok(cursor));
    }

    #[test]
    fn a_refusal_reads_as_its_status_in_every_answer(status in status()) {
        prop_assume!(status != Status::Ok);
        let refused = wire::status_answer(status);
        prop_assert_eq!(wire::read_watch_answer(&refused), Ok(Err(status)));
        prop_assert_eq!(wire::read_send_answer(&refused), Ok(Err(status)));
        prop_assert_eq!(wire::read_sync_answer(&refused), Ok(Err(status)));
    }

    #[test]
    fn a_sync_page_round_trips(
        entries in prop::collection::vec(entry(), 0..8),
        more in any::<bool>(),
        first in u64s(),
        left in prop::option::of(u64s()),
    ) {
        let page = Page { entries, more, first, left };
        prop_assert_eq!(wire::read_sync_answer(&wire::sync_answer(&page)), Ok(Ok(page)));
    }

    #[test]
    fn a_submission_and_its_answer_round_trip(
        ids in any::<([u8; 16], [u8; 16])>(),
        title in bytes(),
        content in bytes(),
        fields in fields(),
        id in any::<[u8; 32]>(),
    ) {
        let submission = Submission {
            client_id: ids.0,
            destination: ids.1,
            title,
            content,
            fields: fields
                .into_iter()
                .map(|(key, value)| (key, msgpack::read_bin(&value, &mut 0).expect("bin").to_vec()))
                .collect(),
        };
        prop_assert_eq!(
            wire::read_send_request(&wire::send_request(&submission)),
            Ok(submission)
        );
        prop_assert_eq!(wire::read_send_answer(&wire::send_answer(&id)), Ok(Ok(id)));
    }

    #[test]
    fn a_mark_of_one_to_the_most_ids_round_trips(
        read in any::<bool>(),
        ids in prop::collection::vec(any::<[u8; 32]>(), 1..=wire::MARK_IDS_MAX),
    ) {
        let flag = if read { MarkFlag::Read } else { MarkFlag::Deleted };
        let mark = Mark { flag, ids };
        prop_assert_eq!(wire::read_mark(&wire::mark_request(&mark)), Ok(mark));
    }

    #[test]
    fn a_mark_of_none_or_too_many_is_refused(read in any::<bool>(), over in 1..8usize) {
        let flag = if read { MarkFlag::Read } else { MarkFlag::Deleted };
        for ids in [Vec::new(), vec![[7; 32]; wire::MARK_IDS_MAX + over]] {
            let mark = Mark { flag, ids };
            prop_assert!(wire::read_mark(&wire::mark_request(&mark)).is_err());
        }
    }

    #[test]
    fn a_peer_state_round_trips_a_mute_at_or_below_zero_as_none(
        peer in any::<[u8; 16]>(),
        flags in any::<(bool, bool, bool)>(),
        muted_until in prop::option::of(i64s()),
    ) {
        let state = PeerState {
            blocked: flags.0,
            pinned: flags.1,
            archived: flags.2,
            muted_until,
        };
        let read = wire::read_peer_state_entry(&wire::peer_state_entry(&peer, &state));
        let want = PeerState {
            muted_until: muted_until.filter(|&until| until > 0),
            ..state
        };
        prop_assert_eq!(read, Ok((peer, want)));
    }

    #[test]
    fn an_imported_row_round_trips_in_either_version(
        origin in text(),
        peer in any::<[u8; 16]>(),
        row in import_row(),
    ) {
        let entry = loquax::import::archive::entry(&origin, &peer, &row);
        let read = loquax::import::archive::read_entry(&entry);
        prop_assert_eq!(read, Some((origin, peer, row)));
    }

    #[test]
    fn the_stored_fields_round_trip(fields in fields()) {
        let packed = loquax::telemetry::pack_fields(&fields);
        prop_assert_eq!(loquax::telemetry::unpack_fields(&packed), Some(fields));
    }

    #[test]
    fn an_admission_code_round_trips_with_and_without_its_server(
        public_key in any::<[u8; 32]>().prop_flat_map(|a| any::<[u8; 32]>().prop_map(move |b| {
            let mut key = [0; 64];
            key[..32].copy_from_slice(&a);
            key[32..].copy_from_slice(&b);
            key
        })),
        token in any::<[u8; 16]>(),
        port in any::<u16>(),
    ) {
        let code = HomeCode { public_key, token };
        prop_assert_eq!(HomeCode::parse(&code.encode()), Ok(code.clone()));
        let server = format!("homenode.lan:{port}");
        prop_assert_eq!(
            HomeCode::parse_with_server(&code.encode_with_server(&server)),
            Ok((code, Some(server)))
        );
    }
}
