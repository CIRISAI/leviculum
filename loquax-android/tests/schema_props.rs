//! Every payload the app's Java and the library exchange
//! (`loquax_android::schema`), encoded and decoded again over the whole
//! range of each field (1173): integers at their extremes (`i64::MIN`,
//! `i64::MAX`, `u32::MAX`, `u8::MAX`), empty strings, strings of 64 KiB,
//! empty and full vectors, every variant of the tagged enums. 256 cases
//! each from a fixed seed, so a red case is the same on every host.
//!
//! The keys get one more: a message key crosses as Java's `long`
//! (1166), which `MsgPack.java` always writes as `d3` and eight bytes, so
//! every `i64` in those bytes must decode to itself in the requests that
//! carry one, and the model key behind it must come back whole.

use loquax_android::schema::{
    self, AddressRow, Addresses, Backup, Blob, CandidateRow, Chip, ContactCopy, ConversationRow,
    Conversations, Detail, Done, Event, FriendRow, Friends, ImRow, Info, LinkRow, Links, MapMark,
    MapRequest, MapShown, Me, MessageRow, Messages, PositionRow, Query, Recents, RegionRow,
    Request, Run, RunRow, Shared, SharedRow, Start, Started, SuggestionRow, Suggestions,
};
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

fn opt_text() -> impl Strategy<Value = Option<String>> {
    prop::option::of(text())
}

fn texts() -> impl Strategy<Value = Vec<String>> {
    prop_oneof![Just(Vec::new()), prop::collection::vec(text(), 1..4)]
}

fn blob() -> impl Strategy<Value = Blob> {
    prop_oneof![
        Just(Vec::new()),
        prop::collection::vec(any::<u8>(), 0..64),
        Just(vec![0xa5; LONG]),
    ]
    .prop_map(Blob)
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

fn i32s() -> impl Strategy<Value = i32> {
    prop_oneof![Just(0), Just(i32::MIN), Just(i32::MAX), any::<i32>()]
}

fn u32s() -> impl Strategy<Value = u32> {
    prop_oneof![Just(0), Just(u32::MAX), any::<u32>()]
}

fn u8s() -> impl Strategy<Value = u8> {
    prop_oneof![Just(0), Just(u8::MAX), any::<u8>()]
}

fn round_trips<T>(value: &T) -> Result<(), TestCaseError>
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = schema::encode(value);
    let back: T = schema::decode(&bytes).map_err(TestCaseError::fail)?;
    prop_assert_eq!(&back, value);
    Ok(())
}

fn start() -> impl Strategy<Value = Start> {
    (
        (text(), opt_text(), opt_text(), opt_text()),
        (i32s(), any::<bool>(), opt_text(), opt_text()),
    )
        .prop_map(
            |((dir, server, code, name), (offset, here, restore, passphrase))| Start {
                dir,
                server,
                code,
                name,
                offset,
                here,
                restore,
                passphrase,
            },
        )
}

fn started() -> impl Strategy<Value = Started> {
    (
        (u32s(), any::<bool>(), opt_text(), opt_text()),
        (opt_text(), opt_text(), any::<bool>(), any::<bool>()),
    )
        .prop_map(
            |((v, ok, error, need), (address, server, link, here))| Started {
                v,
                ok,
                error,
                need,
                address,
                server,
                link,
                here,
            },
        )
}

fn contact() -> impl Strategy<Value = ContactCopy> {
    (text(), i64s(), text(), prop::option::of(blob())).prop_map(
        |(lookup, contact_id, name, photo)| ContactCopy {
            lookup,
            contact_id,
            name,
            photo,
        },
    )
}

fn keys() -> impl Strategy<Value = Vec<i64>> {
    prop_oneof![
        Just(Vec::new()),
        prop::collection::vec(i64s(), 1..8),
        Just(vec![i64::MIN, -1, 0, 1, i64::MAX]),
    ]
}

fn request() -> impl Strategy<Value = Request> {
    prop_oneof![
        text().prop_map(|peer| Request::Open { peer }),
        Just(Request::Close),
        (text(), text()).prop_map(|(peer, body)| Request::Send { peer, body }),
        (text(), text(), text()).prop_map(|(peer, target, content)| Request::React {
            peer,
            target,
            content
        }),
        (text(), text(), text()).prop_map(|(peer, target, body)| Request::Reply {
            peer,
            target,
            body
        }),
        (text(), i64s(), text()).prop_map(|(peer, key, to)| Request::Forward { peer, key, to }),
        (text(), keys()).prop_map(|(peer, keys)| Request::Delete { peer, keys }),
        text().prop_map(|emoji| Request::Used { emoji }),
        Just(Request::Pull),
        any::<bool>().prop_map(|front| Request::Front { front }),
        text().prop_map(|name| Request::Name { name }),
        (text(), contact()).prop_map(|(peer, contact)| Request::Link { peer, contact }),
        text().prop_map(|peer| Request::Unlink { peer }),
        text().prop_map(|peer| Request::NotNow { peer }),
        (text(), text()).prop_map(|(peer, name)| Request::Rename { peer, name }),
        prop::collection::vec(
            (text(), contact()).prop_map(|(address, contact)| ImRow { address, contact }),
            0..4
        )
        .prop_map(|rows| Request::Relink { rows }),
        (text(), any::<bool>()).prop_map(|(peer, on)| Request::Pin { peer, on }),
        (text(), i64s()).prop_map(|(peer, secs)| Request::Mute { peer, secs }),
        (text(), any::<bool>()).prop_map(|(peer, on)| Request::Archive { peer, on }),
        (text(), any::<bool>()).prop_map(|(peer, on)| Request::Block { peer, on }),
        (text(), any::<bool>()).prop_map(|(peer, on)| Request::Verify { peer, on }),
        (text(), any::<bool>()).prop_map(|(peer, on)| Request::Unread { peer, on }),
        (text(), any::<bool>()).prop_map(|(peer, everywhere)| Request::Forget { peer, everywhere }),
        (text(), i32s(), i32s(), i32s(), u32s(), i64s()).prop_map(
            |(peer, lat_e6, lon_e6, alt_e2, accuracy_e2, at)| Request::Share {
                peer,
                lat_e6,
                lon_e6,
                alt_e2,
                accuracy_e2,
                at,
            }
        ),
    ]
}

fn candidate() -> impl Strategy<Value = CandidateRow> {
    ((text(), text(), text()), (text(), text(), texts(), i64s())).prop_map(
        |((key, display, given), (middle, family, nicknames, last_contacted))| CandidateRow {
            key,
            display,
            given,
            middle,
            family,
            nicknames,
            last_contacted,
        },
    )
}

fn query() -> impl Strategy<Value = Query> {
    prop_oneof![
        Just(Query::Conversations),
        text().prop_map(|peer| Query::Messages { peer }),
        (text(), i64s()).prop_map(|(peer, key)| Query::Info { peer, key }),
        Just(Query::Recents),
        (text(), prop::collection::vec(candidate(), 0..4))
            .prop_map(|(peer, candidates)| Query::Suggest { peer, candidates }),
        text().prop_map(|lookup| Query::Contact { lookup }),
        Just(Query::Links),
        text().prop_map(|peer| Query::Detail { peer }),
        Just(Query::Archived),
        Just(Query::Blocked),
        Just(Query::Me),
        text().prop_map(|passphrase| Query::Backup { passphrase }),
        (text(), i64s()).prop_map(|(peer, key)| Query::Run { peer, key }),
        (prop::option::of(i32s()), prop::option::of(i32s()))
            .prop_map(|(lat_e6, lon_e6)| Query::Friends { lat_e6, lon_e6 }),
        Just(Query::Shared),
    ]
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        u32s().prop_map(|v| Event::Changed { v }),
        (u32s(), text()).prop_map(|(v, text)| Event::Notice { v, text }),
        (u32s(), text(), text()).prop_map(|(v, state, detail)| Event::Sync { v, state, detail }),
        (u32s(), any::<bool>()).prop_map(|(v, up)| Event::Link { v, up }),
        (u32s(), text()).prop_map(|(v, detail)| Event::Stopped { v, detail }),
    ]
}

fn chip() -> impl Strategy<Value = Chip> {
    (text(), u32s(), any::<bool>()).prop_map(|(content, count, ours)| Chip {
        content,
        count,
        ours,
    })
}

fn message_row() -> impl Strategy<Value = MessageRow> {
    (
        (text(), any::<bool>(), text(), text(), i64s()),
        (
            prop::collection::vec(chip(), 0..3),
            prop::option::of(i32s()),
            prop::option::of(i32s()),
            opt_text(),
            opt_text(),
        ),
        (prop::option::of(i64s()), prop::option::of(i64s()), i64s()),
        (opt_text(), prop::option::of(any::<bool>()), opt_text()),
        (u32s(), i64s()),
    )
        .prop_map(
            |(
                (id, ours, body, state, at),
                (chips, lat_e6, lon_e6, place, accuracy),
                (fix_at, stale_at, key),
                (quote, quote_ours, quote_to),
                (updates, since),
            )| MessageRow {
                id,
                ours,
                body,
                state,
                at,
                chips,
                lat_e6,
                lon_e6,
                place,
                accuracy,
                fix_at,
                stale_at,
                key,
                quote,
                quote_ours,
                quote_to,
                updates,
                since,
            },
        )
}

fn messages() -> impl Strategy<Value = Messages> {
    (
        (
            u32s(),
            text(),
            text(),
            prop::collection::vec(message_row(), 0..3),
        ),
        (text(), opt_text(), opt_text(), any::<bool>(), opt_text()),
        (
            any::<bool>(),
            opt_text(),
            prop::option::of(i64s()),
            any::<bool>(),
        ),
        (opt_text(), opt_text(), texts(), any::<bool>()),
    )
        .prop_map(
            |(
                (v, peer, name, messages),
                (initials, icon_fg, icon_bg, home_client, photo),
                (linked, lookup, contact_id, ask_link),
                (announced, petname, others, verified),
            )| Messages {
                v,
                peer,
                name,
                messages,
                initials,
                icon_fg,
                icon_bg,
                home_client,
                photo,
                linked,
                lookup,
                contact_id,
                ask_link,
                announced,
                petname,
                others,
                verified,
            },
        )
}

fn conversation_row() -> impl Strategy<Value = ConversationRow> {
    (
        (text(), text(), text(), any::<bool>(), i64s(), u32s()),
        (text(), opt_text(), opt_text(), text(), opt_text()),
        (text(), opt_text(), any::<(bool, bool, bool)>(), u32s()),
    )
        .prop_map(
            |(
                (peer, name, last, ours, at, unread),
                (initials, icon_fg, icon_bg, kind, reaction),
                (state, photo, (linked, pinned, muted), updates),
            )| ConversationRow {
                peer,
                name,
                last,
                ours,
                at,
                unread,
                initials,
                icon_fg,
                icon_bg,
                kind,
                reaction,
                state,
                photo,
                linked,
                pinned,
                muted,
                updates,
            },
        )
}

fn conversations() -> impl Strategy<Value = Conversations> {
    (
        u32s(),
        prop::collection::vec(conversation_row(), 0..3),
        text(),
        text(),
        u32s(),
        any::<(bool, bool)>(),
    )
        .prop_map(
            |(v, conversations, address, initials, archived, (home_client, home_link))| {
                Conversations {
                    v,
                    conversations,
                    address,
                    initials,
                    archived,
                    home_client,
                    home_link,
                }
            },
        )
}

fn detail() -> impl Strategy<Value = Detail> {
    (
        (u32s(), text(), text(), text(), opt_text(), opt_text()),
        (
            opt_text(),
            any::<bool>(),
            opt_text(),
            prop::option::of(i64s()),
            opt_text(),
        ),
        (opt_text(), text(), u32s(), blob()),
        (
            any::<(bool, bool, bool, bool, bool)>(),
            i64s(),
            i64s(),
            i64s(),
        ),
        (
            text(),
            any::<bool>(),
            prop::collection::vec(
                (i64s(), i32s(), i32s()).prop_map(|(at, lat_e6, lon_e6)| PositionRow {
                    at,
                    lat_e6,
                    lon_e6,
                }),
                0..3,
            ),
        ),
    )
        .prop_map(
            |(
                (v, peer, name, initials, icon_fg, icon_bg),
                (photo, linked, lookup, contact_id, petname),
                (announced, fingerprint, qr_size, qr),
                (
                    (verified, blocked, pinned, archived, muted),
                    muted_until,
                    last_received,
                    last_sent,
                ),
                (last_sent_how, home_client, positions),
            )| Detail {
                v,
                peer,
                name,
                initials,
                icon_fg,
                icon_bg,
                photo,
                linked,
                lookup,
                contact_id,
                petname,
                announced,
                fingerprint,
                qr_size,
                qr,
                verified,
                blocked,
                pinned,
                archived,
                muted,
                muted_until,
                last_received,
                last_sent,
                last_sent_how,
                home_client,
                positions,
            },
        )
}

fn map_request_1200() -> BoxedStrategy<MapRequest> {
    (
        (text(), u32s(), u32s(), u32s(), any::<bool>()),
        (
            prop::option::of(u32s()),
            prop::option::of(u32s()),
            prop::option::of(u8s()),
            prop::option::of(u32s()),
            opt_text(),
        ),
        (i32s(), i32s(), i32s(), i32s(), i32s(), i32s()),
        (
            prop::option::of(i32s()),
            prop::option::of(i32s()),
            any::<bool>(),
            prop::option::of(i32s()),
            prop::option::of(i32s()),
        ),
        (any::<bool>(), u32s(), u32s()),
        any::<(bool, bool, bool, bool)>(),
        (opt_text(), any::<bool>(), any::<bool>()),
    )
        .prop_map(
            |(
                (dir, w, h, density_milli, dark),
                (x, y, zoom, zoom_milli, selected),
                (pan_x, pan_y, zoom_by, zoom_by_milli, about_x, about_y),
                (tap_x, tap_y, fit, at_lat_e6, at_lon_e6),
                (load, margin_x, margin_y),
                (moving, toggle_theme, follow_phone, prefetch),
                (set_theme, ask_theme, avatars),
            )| MapRequest {
                dir,
                w,
                h,
                density_milli,
                dark,
                x,
                y,
                zoom,
                zoom_milli,
                selected,
                pan_x,
                pan_y,
                zoom_by,
                zoom_by_milli,
                about_x,
                about_y,
                tap_x,
                tap_y,
                fit,
                at_lat_e6,
                at_lon_e6,
                load,
                margin_x,
                margin_y,
                moving,
                toggle_theme,
                follow_phone,
                set_theme,
                ask_theme,
                prefetch,
                avatars,
                ..MapRequest::default()
            },
        )
        .boxed()
}

/// The map request's fields of 1201 over [`map_request_1200`]'s, boxed
/// apart: one tuple of all of them overflows the test thread's stack.
fn map_request() -> impl Strategy<Value = MapRequest> {
    (
        map_request_1200(),
        (
            prop::option::of(u32s()),
            prop::option::of(i32s()),
            prop::option::of(i32s()),
            any::<bool>(),
            opt_text(),
            prop::option::of(any::<bool>()),
        )
            .boxed(),
    )
        .prop_map(
            |(
                request,
                (at_zoom_milli, own_lat_e6, own_lon_e6, ask_regions, delete_region, set_autoload),
            )| MapRequest {
                at_zoom_milli,
                own_lat_e6,
                own_lon_e6,
                ask_regions,
                delete_region,
                set_autoload,
                ..request
            },
        )
}

fn map_shown_1200() -> BoxedStrategy<MapShown> {
    (
        (u32s(), any::<bool>(), opt_text(), u32s(), u32s(), u32s()),
        (u32s(), u32s(), u32s(), u8s(), u32s(), u8s()),
        (any::<bool>(), u8s(), u8s(), opt_text(), text(), u32s()),
        (opt_text(), opt_text(), any::<bool>(), opt_text(), u32s()),
        (any::<(bool, bool, bool)>(), u32s(), text()),
        (marks(), any::<(bool, bool)>()),
    )
        .prop_map(
            |(
                (v, ok, error, w, h, margin_x),
                (margin_y, x, y, zoom, zoom_milli, tile_zoom),
                (pending, min_zoom, max_zoom, selected, attribution, pins),
                (tapped, tapped_name, loading, notice, ms),
                ((dark, theme_chosen, prefetching), prefetched, theme),
                (marks, (offline, no_map)),
            )| MapShown {
                v,
                ok,
                error,
                w,
                h,
                margin_x,
                margin_y,
                x,
                y,
                zoom,
                zoom_milli,
                tile_zoom,
                pending,
                min_zoom,
                max_zoom,
                selected,
                attribution,
                pins,
                tapped,
                tapped_name,
                loading,
                notice,
                ms,
                dark,
                theme_chosen,
                theme,
                prefetching,
                prefetched,
                marks,
                offline,
                no_map,
                ..MapShown::default()
            },
        )
        .boxed()
}

/// The frame's fields of 1201 over [`map_shown_1200`]'s, boxed apart as
/// for the request.
fn map_shown() -> impl Strategy<Value = MapShown> {
    (
        map_shown_1200(),
        (
            prop::option::of(i32s()),
            prop::option::of(i32s()),
            regions(),
            any::<u64>(),
            any::<bool>(),
        )
            .boxed(),
    )
        .prop_map(
            |(shown, (own_x, own_y, regions, region_bytes, autoload))| MapShown {
                own_x,
                own_y,
                regions,
                region_bytes,
                autoload,
                ..shown
            },
        )
}

fn regions() -> impl Strategy<Value = Vec<RegionRow>> {
    prop::collection::vec(
        (
            (text(), any::<u64>(), i64s()),
            (i32s(), i32s(), u8s(), u8s(), any::<bool>()),
        )
            .prop_map(
                |((name, bytes, modified), (lat_e6, lon_e6, min_zoom, max_zoom, auto))| RegionRow {
                    name,
                    bytes,
                    modified,
                    lat_e6,
                    lon_e6,
                    min_zoom,
                    max_zoom,
                    auto,
                },
            ),
        0..3,
    )
}

fn marks() -> impl Strategy<Value = Vec<MapMark>> {
    prop::collection::vec(
        (i32s(), i32s(), texts(), prop::option::of(u32s())).prop_map(
            |(x, y, peers, split_milli)| MapMark {
                x,
                y,
                peers,
                split_milli,
            },
        ),
        0..4,
    )
}

fn friend_row() -> impl Strategy<Value = FriendRow> {
    (
        (text(), text(), text(), opt_text(), opt_text(), opt_text()),
        (i64s(), i32s(), i32s(), prop::option::of(u32s())),
        (any::<(bool, bool)>(), text()),
    )
        .prop_map(
            |(
                (peer, name, initials, icon_fg, icon_bg, photo),
                (at, lat_e6, lon_e6, distance_m),
                ((recent, stale), geo),
            )| FriendRow {
                peer,
                name,
                initials,
                icon_fg,
                icon_bg,
                photo,
                at,
                lat_e6,
                lon_e6,
                distance_m,
                recent,
                stale,
                geo,
            },
        )
}

/// A request map as `MsgPack.pack` writes it: every `long` as `d3` and
/// eight bytes, a string as `str` 8, 16 or 32.
fn java_bytes(t: &str, peer: &str, longs: &[(&str, &[i64])]) -> Vec<u8> {
    fn string(out: &mut Vec<u8>, s: &str) {
        let n = s.len();
        if n < 0x100 {
            out.extend_from_slice(&[0xd9, n as u8]);
        } else if n < 0x1_0000 {
            out.push(0xda);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            out.push(0xdb);
            out.extend_from_slice(&(n as u32).to_be_bytes());
        }
        out.extend_from_slice(s.as_bytes());
    }
    let mut out = vec![0x80 | (2 + longs.len()) as u8];
    for (name, value) in [("t", t), ("peer", peer)] {
        string(&mut out, name);
        string(&mut out, value);
    }
    for (name, values) in longs {
        string(&mut out, name);
        let one = matches!(*name, "key");
        if !one {
            out.push(0xdd);
            out.extend_from_slice(&(values.len() as u32).to_be_bytes());
        }
        for n in *values {
            out.push(0xd3);
            out.extend_from_slice(&n.to_be_bytes());
        }
    }
    out
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn a_start_and_its_answer_round_trip(start in start(), started in started()) {
        round_trips(&start)?;
        round_trips(&started)?;
    }

    #[test]
    fn every_request_round_trips(request in request()) {
        round_trips(&request)?;
    }

    #[test]
    fn every_query_round_trips(query in query()) {
        round_trips(&query)?;
    }

    #[test]
    fn every_event_round_trips(event in event()) {
        round_trips(&event)?;
    }

    #[test]
    fn the_lists_round_trip(conversations in conversations(), messages in messages()) {
        round_trips(&conversations)?;
        round_trips(&messages)?;
    }

    #[test]
    fn a_contact_screen_round_trips(detail in detail()) {
        round_trips(&detail)?;
    }

    #[test]
    fn the_small_answers_round_trip(
        v in u32s(),
        rows in texts(),
        error in opt_text(),
        ok in any::<bool>(),
        line in text(),
        qr in blob(),
        qr_size in u32s(),
        names in (text(), text(), opt_text(), i64s(), u32s()),
    ) {
        round_trips(&Info { v, rows: rows.clone() })?;
        round_trips(&Recents { v, emoji: rows.clone() })?;
        round_trips(&Done { v, ok, error: error.clone() })?;
        round_trips(&Backup { v, line: line.clone(), qr_size, qr: qr.clone() })?;
        round_trips(&Me {
            v,
            address: names.0.clone(),
            fingerprint: names.1.clone(),
            qr_size,
            qr,
            name: line.clone(),
            here: ok,
        })?;
        round_trips(&Suggestions {
            v,
            suggestions: vec![SuggestionRow {
                key: names.0.clone(),
                score: names.4,
                field: names.1.clone(),
                nickname: names.2.clone(),
            }],
        })?;
        round_trips(&Addresses {
            v,
            addresses: vec![AddressRow {
                peer: names.0.clone(),
                name: names.1.clone(),
                announced: error,
            }],
        })?;
        round_trips(&Links {
            v,
            links: vec![LinkRow {
                peer: names.0,
                lookup: line,
                contact_id: names.3,
                name: names.1,
            }],
        })?;
        round_trips(&Info { v, rows: Vec::new() })?;
        round_trips(&Run { v, positions: Vec::new() })?;
    }

    #[test]
    fn a_run_round_trips(
        v in u32s(),
        rows in prop::collection::vec(
            (
                i64s(),
                i64s(),
                prop::option::of(i32s()),
                prop::option::of(i32s()),
                opt_text(),
                opt_text(),
                prop::option::of(i64s()),
            ),
            0..4,
        ),
    ) {
        let positions = rows
            .into_iter()
            .map(|(key, at, lat_e6, lon_e6, place, accuracy, fix_at)| RunRow {
                key,
                at,
                lat_e6,
                lon_e6,
                place,
                accuracy,
                fix_at,
            })
            .collect();
        round_trips(&Run { v, positions })?;
    }

    #[test]
    fn a_map_request_and_its_frame_round_trip(
        request in map_request(),
        shown in map_shown(),
        rgba in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        round_trips(&request)?;
        round_trips(&shown)?;
        let framed = schema::frame(&shown, &rgba);
        let (back, pixels) = schema::unframe(&framed).map_err(TestCaseError::fail)?;
        prop_assert_eq!(back, shown);
        prop_assert_eq!(pixels, &rgba[..]);
    }

    #[test]
    fn the_friends_round_trip(
        v in u32s(),
        friends in prop::collection::vec(friend_row(), 0..4),
        me in prop::option::of(friend_row()),
    ) {
        round_trips(&Friends { v, friends, me })?;
    }

    #[test]
    fn the_shared_positions_round_trip(
        v in u32s(),
        rows in prop::collection::vec(
            (
                (text(), text(), text(), opt_text(), opt_text(), opt_text()),
                (i64s(), i32s(), i32s()),
            ),
            0..4,
        ),
    ) {
        let shared = rows
            .into_iter()
            .map(|((peer, name, initials, icon_fg, icon_bg, photo), (at, lat_e6, lon_e6))| {
                SharedRow { peer, name, initials, icon_fg, icon_bg, photo, at, lat_e6, lon_e6 }
            })
            .collect();
        round_trips(&Shared { v, shared })?;
    }

    #[test]
    fn a_key_comes_back_whole_both_ways(wire in i64s(), model in any::<u64>()) {
        prop_assert_eq!(schema::wire_key(schema::model_key(wire)), wire);
        prop_assert_eq!(schema::model_key(schema::wire_key(model)), model);
    }

    #[test]
    fn every_key_in_javas_bytes_reaches_its_request(
        peer in text(),
        key in i64s(),
        keys in keys(),
    ) {
        let info: Query = schema::decode(&java_bytes("info", &peer, &[("key", &[key])]))
            .map_err(TestCaseError::fail)?;
        prop_assert_eq!(info, Query::Info { peer: peer.clone(), key });
        let delete: Request = schema::decode(&java_bytes("delete", &peer, &[("keys", &keys)]))
            .map_err(TestCaseError::fail)?;
        prop_assert_eq!(delete, Request::Delete { peer, keys });
    }
}
