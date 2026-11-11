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
    self, AddressRow, Addresses, Backup, Bar, BarButton, Blob, CandidateRow, Chip, ContactCopy,
    ConversationRow, Conversations, Detail, Done, Event, FileRow, FriendRow, Friends, ImRow, Info,
    InfoRow, LinkCode, LinkRow, Linked, Links, MapFlingChoice, MapMark, MapRegions, MapRequest,
    MapShown, MapThemeChoice, Me, MessageRow, Messages, PositionRow, Query, Recents, Recording,
    RegionRow, Request, Run, RunRow, ShareRow, Shared, SharedRow, Shares, Start, Started,
    SuggestionRow, Suggestions,
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
        (opt_text(), any::<bool>()),
        (any::<bool>(), opt_text(), any::<bool>()),
    )
        .prop_map(
            |(
                (dir, server, code, name),
                (offset, here, restore, passphrase),
                (lang, camera),
                (anyway, moving, take_back),
            )| {
                Start {
                    dir,
                    server,
                    code,
                    name,
                    offset,
                    lang,
                    here,
                    restore,
                    passphrase,
                    anyway,
                    moving,
                    take_back,
                    camera,
                }
            },
        )
}

fn started() -> impl Strategy<Value = Started> {
    (
        (u32s(), any::<bool>(), opt_text(), opt_text(), opt_text()),
        (opt_text(), opt_text(), any::<bool>(), any::<bool>()),
    )
        .prop_map(
            |((v, ok, error, need, details), (address, server, link, here))| Started {
                v,
                ok,
                error,
                need,
                details,
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
        (text(), i64s()).prop_map(|(peer, key)| Request::SendAgain { peer, key }),
        (text(), blob()).prop_map(|(peer, data)| Request::Photo { peer, data }),
        (text(), text(), blob(), any::<u64>()).prop_map(|(peer, name, data, size)| Request::File {
            peer,
            name,
            data,
            size
        }),
        (text(), blob(), u32s()).prop_map(|(peer, pcm, rate)| Request::Voice { peer, pcm, rate }),
        (text(), i64s(), any::<bool>()).prop_map(|(peer, key, on)| Request::PlayVoice {
            peer,
            key,
            on
        }),
        (text(), keys()).prop_map(|(peer, keys)| Request::Delete { peer, keys }),
        text().prop_map(|emoji| Request::Used { emoji }),
        Just(Request::Pull),
        any::<bool>().prop_map(|front| Request::Front { front }),
        (text(), keys()).prop_map(|(peer, keys)| Request::Viewport { peer, keys }),
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
        (text(), text(), text()).prop_map(|(peer, body, reply)| Request::Draft {
            peer,
            body,
            reply
        }),
        (text(), blob()).prop_map(|(peer, data)| Request::Stage { peer, data }),
        text().prop_map(|peer| Request::Unstage { peer }),
        (text(), text()).prop_map(|(peer, kind)| Request::Sheet { peer, kind }),
        (i32s(), i32s(), i32s(), u32s(), i64s()).prop_map(
            |(lat_e6, lon_e6, alt_e2, accuracy_e2, at)| Request::Fix {
                lat_e6,
                lon_e6,
                alt_e2,
                accuracy_e2,
                at,
            }
        ),
        Just(Request::NoFix),
        text().prop_map(|peer| Request::ShareFix { peer }),
        (text(), text()).prop_map(|(peer, span)| Request::StartShare { peer, span }),
        text().prop_map(|peer| Request::StopShare { peer }),
        ((text(), text()), (i64s(), i32s(), i32s(), u32s())).prop_map(
            |((peer, input), (now, dx, dy, rms))| Request::Record {
                peer,
                input,
                now,
                dx,
                dy,
                rms,
            }
        ),
        (text(), text()).prop_map(|(peer, verb)| Request::Recording { peer, verb }),
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
        opt_text().prop_map(|search| Query::Conversations { search }),
        text().prop_map(|peer| Query::Messages { peer }),
        (text(), i64s()).prop_map(|(peer, key)| Query::Info { peer, key }),
        Just(Query::Recents),
        (text(), prop::collection::vec(candidate(), 0..4))
            .prop_map(|(peer, candidates)| Query::Suggest { peer, candidates }),
        text().prop_map(|lookup| Query::Contact { lookup }),
        Just(Query::Links),
        text().prop_map(|peer| Query::Detail { peer }),
        opt_text().prop_map(|search| Query::Archived { search }),
        Just(Query::Blocked),
        Just(Query::Me),
        text().prop_map(|passphrase| Query::Backup { passphrase }),
        (text(), i64s()).prop_map(|(peer, key)| Query::Run { peer, key }),
        (prop::option::of(i32s()), prop::option::of(i32s()))
            .prop_map(|(lat_e6, lon_e6)| Query::Friends { lat_e6, lon_e6 }),
        Just(Query::Shared),
        Just(Query::Profile),
        Just(Query::LinkDevice),
        Just(Query::Linked),
        Just(Query::MovePhone),
        Just(Query::Moving),
        (text(), opt_text()).prop_map(|(text, lang)| Query::Lxma { text, lang }),
        (text(), opt_text()).prop_map(|(text, lang)| Query::Code { text, lang }),
        (opt_text(), prop::option::of(any::<bool>())).prop_map(|(delete, set_autoload)| {
            Query::MapRegions {
                delete,
                set_autoload,
            }
        }),
        opt_text().prop_map(|set| Query::MapTheme { set }),
        prop::option::of(any::<bool>()).prop_map(|set| Query::MapFling { set }),
        opt_text().prop_map(|set| Query::PhotoSize { set }),
        (text(), i64s(), u32s()).prop_map(|(peer, key, edge)| Query::Photo { peer, key, edge }),
        (text(), u32s()).prop_map(|(peer, edge)| Query::Staged { peer, edge }),
        text().prop_map(|peer| Query::Bar { peer }),
        Just(Query::Shares),
        (text(), i64s(), u32s()).prop_map(|(peer, key, index)| Query::SaveFile {
            peer,
            key,
            index
        }),
        (text(), i64s()).prop_map(|(peer, key)| Query::Voice { peer, key }),
        text().prop_map(|peer| Query::Recording { peer }),
    ]
}

fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        (u32s(), any::<u64>()).prop_map(|(v, counts)| Event::Changed { v, counts }),
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

fn file_row() -> impl Strategy<Value = FileRow> {
    (text(), u32s(), text(), text()).prop_map(|(name, bytes, size, words)| FileRow {
        name,
        bytes,
        size,
        words,
    })
}

/// A row's voice message (1222), boxed on its own: one more tuple inline
/// overflowed the test thread's stack in [`the_lists_round_trip`].
type VoiceParts = (u32, u32, String, Blob, (bool, bool), String);

fn voice_parts() -> BoxedStrategy<VoiceParts> {
    (
        u32s(),
        u32s(),
        text(),
        blob(),
        any::<(bool, bool)>(),
        text(),
    )
        .boxed()
}

/// A bubble's voice message and its bytes under way (1662), boxed
/// together: an eleventh element of the row's tuple overflowed the test
/// thread's stack in [`the_lists_round_trip`].
fn media_parts() -> BoxedStrategy<(VoiceParts, (String, u32))> {
    (voice_parts(), (text(), u32s())).boxed()
}

/// A bubble's group parts (1307): its writer and an invitation's card,
/// boxed on their own as [`voice_parts`] is.
type InviteParts = (String, String, String, String, String, bool);

fn invite_parts() -> BoxedStrategy<InviteParts> {
    (text(), text(), text(), text(), text(), any::<bool>()).boxed()
}

/// A group's line parts (1307 pass four): the note line's flag and the
/// late words, boxed on their own as the others are; 1449 adds the
/// mark's words.
fn line_parts() -> BoxedStrategy<(bool, String, String, String)> {
    (any::<bool>(), text(), text(), text()).boxed()
}

/// A chat's and a row's group parts (1307), boxed the same way.
/// Pass four adds a chat's ended line.
type GroupParts = (u32, String, String, bool, String);

fn group_parts() -> BoxedStrategy<GroupParts> {
    (u32s(), text(), text(), any::<bool>(), text()).boxed()
}

/// The rows are boxed: with the scene's fields (1217) their tuples together
/// overflow the test thread's stack in [`the_lists_round_trip`].
fn message_row() -> BoxedStrategy<MessageRow> {
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
        (u32s(), i64s(), i64s()),
        (text(), u32s()),
        (any::<(bool, bool, bool, bool)>(), text(), text()),
        (text(), u32s(), u32s(), u32s(), text()),
        (
            prop::collection::vec(file_row(), 0..3),
            invite_parts(),
            line_parts(),
        ),
        media_parts(),
    )
        .prop_map(
            |(
                (id, ours, body, state, at),
                (chips, lat_e6, lon_e6, place, accuracy),
                (fix_at, stale_at, key),
                (quote, quote_ours, quote_to),
                (updates, since, read_at),
                (liveness, cadence_s),
                ((joins_before, joins_after, meta, newest), day, live),
                (photo, photo_bytes, photo_w, photo_h, photo_words),
                (
                    files,
                    (writer, invite_group, invite_title, invite_trust, invite_state, invite_join),
                    (note, late, state_words, position_words),
                ),
                (
                    (
                        voice_mode,
                        voice_ms,
                        voice_duration,
                        voice_levels,
                        (voice_playable, voice_playing),
                        voice_words,
                    ),
                    (progress_words, progress_percent),
                ),
            )| MessageRow {
                id,
                ours,
                body,
                state,
                read_at,
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
                liveness,
                cadence_s,
                joins_before,
                joins_after,
                meta,
                newest,
                day,
                live,
                photo,
                photo_bytes,
                photo_w,
                photo_h,
                photo_words,
                files,
                progress_words,
                progress_percent,
                voice_mode,
                voice_ms,
                voice_duration,
                voice_levels,
                voice_playable,
                voice_playing,
                voice_words,
                writer,
                invite_group,
                invite_title,
                invite_trust,
                invite_state,
                invite_join,
                note,
                late,
                state_words,
                position_words,
            },
        )
        .boxed()
}

/// The head of [`Messages`], boxed on its own as [`voice_parts`] is: with
/// a relay's title (1482) inline the tuple overflowed the test thread's
/// stack in [`the_lists_round_trip`].
type MessagesHead = (u32, String, String, Vec<MessageRow>, String);

fn messages_head() -> BoxedStrategy<MessagesHead> {
    (
        u32s(),
        text(),
        text(),
        prop::collection::vec(message_row(), 0..3),
        text(),
    )
        .boxed()
}

fn messages() -> BoxedStrategy<Messages> {
    (
        messages_head(),
        (text(), opt_text(), opt_text(), any::<bool>(), opt_text()),
        (
            any::<bool>(),
            opt_text(),
            prop::option::of(i64s()),
            any::<bool>(),
        ),
        (
            opt_text(),
            opt_text(),
            texts(),
            any::<bool>(),
            any::<bool>(),
        ),
        live(),
        (text(), text(), text()),
        // The group's count and the invited card (1307).
        group_parts(),
        bar(),
        share_chip(),
    )
        .prop_map(
            |(
                (v, peer, name, messages, relayed),
                (initials, icon_fg, icon_bg, home_client, photo),
                (linked, lookup, contact_id, ask_link),
                (announced, petname, others, verified, receipts),
                (liveness, cadence_s, last_fix_at),
                (ring, draft, draft_reply),
                (group_members, invite_title, invite_trust, _, ended),
                bar,
                (share, share_stop),
            )| Messages {
                v,
                peer,
                // An extreme text the relay's title draws too (1452).
                same_name: relayed.clone(),
                name,
                relayed,
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
                receipts,
                liveness,
                cadence_s,
                last_fix_at,
                ring,
                draft,
                draft_reply,
                round: bar.round,
                buttons: bar.buttons,
                hint: bar.hint,
                staged: bar.staged,
                sheet: bar.sheet,
                sheet_title: bar.sheet_title,
                sheet_rows: bar.sheet_rows,
                has_fix: bar.has_fix,
                fix_lat_e6: bar.fix_lat_e6,
                fix_lon_e6: bar.fix_lon_e6,
                fix_words: bar.fix_words,
                share,
                share_stop,
                recording: bar.recording,
                group_members,
                invite_title,
                invite_trust,
                ended,
            },
        )
        .boxed()
}

/// The recording bar (1276), boxed as [`bar`] is.
fn recording() -> BoxedStrategy<Recording> {
    (
        (u32s(), any::<bool>(), text(), any::<bool>(), any::<bool>()),
        (
            text(),
            u32s(),
            any::<bool>(),
            blob(),
            any::<bool>(),
            text(),
            u32s(),
        ),
        (text(), u32s(), any::<bool>(), text()),
        (text(), any::<bool>(), u32s()),
        (
            prop::collection::vec(bar_button(), 0..5),
            text(),
            text(),
            texts(),
        ),
        (u32s(), u32s(), u32s(), u32s()),
    )
        .prop_map(
            |(
                (v, ok, phase, shown, pulsing),
                (timer, timer_ms, amber, wave, moving, wave_label, wave_slots),
                (cancel, cancel_progress, armed, held),
                (lock, lock_closed, lock_progress),
                (buttons, notice, notice_allow, effects),
                (cancel_dp, lock_dp, window_ms, longest_ms),
            )| Recording {
                v,
                ok,
                phase,
                shown,
                pulsing,
                timer,
                timer_ms,
                amber,
                wave,
                moving,
                wave_label,
                wave_slots,
                cancel,
                cancel_progress,
                armed,
                held,
                lock,
                lock_closed,
                lock_progress,
                buttons,
                notice,
                notice_allow,
                effects,
                cancel_dp,
                lock_dp,
                window_ms,
                longest_ms,
            },
        )
        .boxed()
}

/// The chat's share chip (1273), boxed as [`bar`] is.
fn share_chip() -> BoxedStrategy<(String, String)> {
    (text(), text()).boxed()
}

/// Settings, Privacy's running shares (1273).
fn shares() -> impl Strategy<Value = Shares> {
    (
        (u32s(), text(), text(), text()),
        prop::collection::vec(
            (text(), text(), text(), text(), i64s()).prop_map(|(peer, id, name, words, until)| {
                ShareRow {
                    peer,
                    id,
                    name,
                    words,
                    until,
                }
            }),
            0..3,
        ),
    )
        .prop_map(|((v, title, empty, stop), shares)| Shares {
            v,
            title,
            empty,
            stop,
            shares,
        })
}

/// The compose bar's fields (1270), boxed: inlined into [`messages`] the
/// strategy's type nests deep enough to overflow a test thread's stack.
fn bar() -> BoxedStrategy<Bar> {
    (
        (
            u32s(),
            text(),
            prop::collection::vec(bar_button(), 0..6),
            text(),
            text(),
        ),
        (text(), text(), prop::collection::vec(bar_button(), 0..4)),
        (any::<bool>(), i32s(), i32s(), text(), text()),
    )
        .prop_map(
            |(
                (v, round, buttons, hint, staged),
                (sheet, sheet_title, sheet_rows),
                (has_fix, fix_lat_e6, fix_lon_e6, fix_words, recording),
            )| Bar {
                v,
                round,
                buttons,
                hint,
                staged,
                sheet,
                sheet_title,
                sheet_rows,
                has_fix,
                fix_lat_e6,
                fix_lon_e6,
                fix_words,
                recording,
            },
        )
        .boxed()
}

/// A compose bar's button or a sheet's row (1270).
fn bar_button() -> impl Strategy<Value = BarButton> {
    (text(), text()).prop_map(|(id, label)| BarButton { id, label })
}

/// A liveness as the rows carry it (1209): the word, the cadence and the
/// newest fix's time.
fn live() -> impl Strategy<Value = (String, u32, i64)> {
    (text(), u32s(), i64s())
}

fn conversation_row() -> BoxedStrategy<ConversationRow> {
    (
        (text(), text(), text(), any::<bool>(), i64s(), u32s()),
        (text(), opt_text(), opt_text(), text(), opt_text()),
        (
            text(),
            opt_text(),
            any::<(bool, bool, bool)>(),
            u32s(),
            i64s(),
        ),
        live(),
        (text(), text(), text(), text(), text()),
        any::<bool>(),
        // A group's count and words (1307).
        group_parts(),
    )
        .prop_map(
            |(
                (peer, name, last, ours, at, unread),
                (initials, icon_fg, icon_bg, kind, reaction),
                (state, photo, (linked, pinned, muted), updates, read_at),
                (liveness, cadence_s, last_fix_at),
                (live, ring, pin, pin_label, draft),
                sharing,
                (group_members, group_words, _, group_invited, time),
            )| ConversationRow {
                peer,
                same_name: last.clone(),
                name,
                last,
                ours,
                at,
                time,
                unread,
                initials,
                icon_fg,
                icon_bg,
                kind,
                reaction,
                state,
                read_at,
                photo,
                linked,
                pinned,
                muted,
                updates,
                liveness,
                cadence_s,
                last_fix_at,
                live,
                ring,
                pin,
                pin_label,
                draft,
                sharing,
                group_members,
                group_words,
                group_invited,
            },
        )
        .boxed()
}

fn conversations() -> impl Strategy<Value = Conversations> {
    (
        u32s(),
        prop::collection::vec(conversation_row(), 0..3),
        text(),
        text(),
        u32s(),
        any::<(bool, bool, bool)>(),
    )
        .prop_map(
            |(v, conversations, address, initials, archived, (home_client, home_link, offline))| {
                Conversations {
                    v,
                    conversations,
                    address,
                    initials,
                    archived,
                    home_client,
                    home_link,
                    offline,
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
                (i64s(), i32s(), i32s(), text()).prop_map(|(at, lat_e6, lon_e6, day)| {
                    PositionRow {
                        at,
                        lat_e6,
                        lon_e6,
                        day,
                    }
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
        any::<bool>(),
    )
        .prop_map(
            |(
                (dir, w, h, density_milli, dark),
                (x, y, zoom, zoom_milli, selected),
                (pan_x, pan_y, zoom_by, zoom_by_milli, about_x, about_y),
                (tap_x, tap_y, fit, at_lat_e6, at_lon_e6),
                (load, margin_x, margin_y),
                (moving, toggle_theme, follow_phone, prefetch),
                avatars,
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
        )
            .boxed(),
    )
        .prop_map(
            |(request, (at_zoom_milli, own_lat_e6, own_lon_e6))| MapRequest {
                at_zoom_milli,
                own_lat_e6,
                own_lon_e6,
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

/// The frame's fields of 1201 and 1188 over [`map_shown_1200`]'s, boxed
/// apart as for the request.
fn map_shown() -> impl Strategy<Value = MapShown> {
    (
        map_shown_1200(),
        (
            prop::option::of(i32s()),
            prop::option::of(i32s()),
            any::<bool>(),
        )
            .boxed(),
        (any::<bool>(), u32s(), u32s(), any::<bool>()).boxed(),
        // 1441: the ground beside `filling`.
        (any::<bool>(), u32s()),
        // 1419: the map's title and the time window sheet.
        (
            text(),
            text(),
            prop::collection::vec(
                (text(), text(), any::<bool>()).prop_map(|(word, label, chosen)| {
                    schema::TrackChoice {
                        word,
                        label,
                        chosen,
                    }
                }),
                0..7,
            ),
        )
            .boxed(),
    )
        .prop_map(
            |(
                shown,
                (own_x, own_y, autoload),
                (fling, room_north, room_south, loading_here),
                (filling, ground),
                (track_window, title, track_windows),
            )| {
                MapShown {
                    own_x,
                    own_y,
                    autoload,
                    fling,
                    room_north,
                    room_south,
                    loading_here,
                    filling,
                    ground,
                    track_window,
                    title,
                    track_windows,
                    ..shown
                }
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
        (
            i32s(),
            i32s(),
            texts(),
            prop::option::of(u32s()),
            opt_text(),
            any::<bool>(),
        )
            .prop_map(|(x, y, peers, split_milli, way, low_battery)| MapMark {
                x,
                y,
                peers,
                split_milli,
                way,
                low_battery,
            }),
        0..4,
    )
}

fn friend_row() -> BoxedStrategy<FriendRow> {
    (
        (text(), text(), text(), opt_text(), opt_text(), opt_text()),
        (i64s(), i32s(), i32s(), prop::option::of(u32s())),
        (any::<(bool, bool)>(), text()),
        live(),
        (text(), text(), text()),
        (any::<bool>(), text()),
    )
        .prop_map(
            |(
                (peer, name, initials, icon_fg, icon_bg, photo),
                (at, lat_e6, lon_e6, distance_m),
                ((recent, stale), geo),
                (liveness, cadence_s, last_fix_at),
                (live, age, ring),
                (nearby, announced),
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
                liveness,
                cadence_s,
                last_fix_at,
                live,
                age,
                ring,
                nearby,
                announced,
            },
        )
        .boxed()
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
    fn the_compose_bar_round_trips(bar in bar()) {
        round_trips(&bar)?;
    }

    #[test]
    fn the_running_shares_round_trip(shares in shares()) {
        round_trips(&shares)?;
    }

    #[test]
    fn the_recording_bar_round_trips(recording in recording()) {
        round_trips(&recording)?;
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
        round_trips(&Info {
            v,
            rows: vec![InfoRow { label: line.clone(), value: names.1.clone() }],
            advanced: rows.clone(),
        })?;
        round_trips(&Recents { v, emoji: rows.clone() })?;
        round_trips(&Done { v, ok, error: error.clone() })?;
        round_trips(&Backup { v, line: line.clone(), qr_size, qr: qr.clone() })?;
        round_trips(&LinkCode {
            v,
            code: line.clone(),
            qr_size,
            qr: qr.clone(),
            hours: names.4,
            expires_at: names.3,
        })?;
        round_trips(&Linked { v, state: names.0.clone(), name: names.1.clone() })?;
        round_trips(&Me {
            v,
            address: names.0.clone(),
            fingerprint: names.1.clone(),
            lxma: names.2.clone().unwrap_or_default(),
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
        round_trips(&Info { v, rows: Vec::new(), advanced: Vec::new() })?;
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
    fn the_settings_pages_map_answers_round_trip(
        v in u32s(),
        ok in any::<bool>(),
        regions in regions(),
        region_bytes in any::<u64>(),
        autoload in any::<bool>(),
        theme in text(),
        theme_chosen in any::<bool>(),
    ) {
        round_trips(&MapRegions { v, ok, regions, region_bytes, autoload })?;
        round_trips(&MapThemeChoice { v, ok, theme, theme_chosen })?;
        round_trips(&MapFlingChoice { v, ok, fling: autoload })?;
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
