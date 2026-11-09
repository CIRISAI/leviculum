//! A store shaped like Lew's (1489, structure review 2026-11-07 finding 14,
//! plan step A): 300 conversations and 4000 messages, as a phone holds them
//! after a first pull of a mailbox with 4067 entries. The tests and benches
//! of every target build their model from it (the window, the terminal, the
//! phone's facade, the M9's frames, the core), so a cost that grows with the
//! history shows on all of them alike. Shared by `#[path]` as
//! `ugly_store.rs` is.
//!
//! Where `ugly_store` is ugly (imports, odd names, broken rows), this one is
//! big and ordinary:
//!
//! | conv     | what                                                         | messages |
//! |----------|--------------------------------------------------------------|----------|
//! | 0        | a group of [`GROUP_MEMBERS`] members: 456 copies, 44 ours     | 500      |
//! | 1        | Anna, the long chat, pinned                                  | 1200     |
//! | 2..5     | three friends whose phones report positions every half hour over four days, a few words between | 80 each |
//! | 5..25    | twenty friends                                               | 40 each  |
//! | 25..300  | the rest, a few messages each                                | 4 or 5   |
//!
//! In every conversation (row `j`, by [`Draw`] from the conversation and the
//! row, so the same bytes on every run): bodies of 110 to 330 characters,
//! every 23rd one of 2000 to 4000; every 9th a reply quoting the row before;
//! every 13th a reaction to the row two back; in the long chat and the
//! group every 50th a photo and every 75th a voice message, both real
//! (`../fixtures/real-photo.jpg`, Loquax's own "Small" JPEG of the
//! portrait, 23 KB; `../fixtures/real-voice.ogg`, four seconds of Opus);
//! sent rows delivered, every 7th read. [`UNREAD`] messages are unread in
//! eight conversations, two are archived, one muted. The newest row lies
//! at [`BASE`]; the long chat reaches a year back, the reports four days.
//!
//! Two ways in: [`restored`] hands the model every message (a chat a first
//! pull filled stays whole in memory), [`fill_store_of`] writes the same
//! rows into a store as the screen writes them (a restart then loads the
//! newest 200 of each).
//!
//! [`bar`] and [`KNOWN_REDS`] are the register of the bars that define
//! "only what is visible" and "O(1) per event" (1489 step 3): red at the
//! commit that wrote them, each green with the plan step that fixes it.

#![allow(dead_code)]

use std::path::Path;

use leviculum_lxmf::lev::{GroupCopy, Lev, CAP_GROUPS};
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;
use loquax::store::Store;
use loquax::ui::groups::{GroupInfo, GroupMember, Standing};
use loquax::ui::history::{Draft, Flags, Restored, StoreWrite};
use loquax::ui::{Message, Signature};
use loquax::when::WallTime;

/// How many conversations.
pub const CONVERSATIONS: usize = 300;
/// How many messages in all.
pub const MESSAGES: usize = 4000;
/// The group's conversation.
pub const GROUP: usize = 0;
/// The group's members, the reader among them.
pub const GROUP_MEMBERS: usize = 40;
/// The group's copies.
pub const GROUP_MESSAGES: usize = 500;
/// The long chat, pinned.
pub const ANNA: usize = 1;
/// The long chat's messages.
pub const ANNA_MESSAGES: usize = 1200;
/// The friends who report their position.
pub const REPORTERS: core::ops::Range<usize> = 2..5;
/// Each reporter's messages, [`REPORTS`] of them reports.
pub const REPORTER_MESSAGES: usize = 80;
/// Each reporter's reports, every half hour.
pub const REPORTS: usize = 64;
/// The friends with a chat of their own.
pub const FRIENDS: core::ops::Range<usize> = 5..25;
/// Each friend's messages.
pub const FRIEND_MESSAGES: usize = 40;
/// Unread messages in all.
pub const UNREAD: usize = 20;
/// The archived conversations.
pub const ARCHIVED: [usize; 2] = [40, 41];
/// The conversation muted for always.
pub const MUTED: usize = 6;
/// The newest row: 2026-11-05 17:33 UTC.
pub const BASE: i64 = 1_793_900_000;
/// What every draw starts from.
pub const SEED: u64 = 0x1489_0000_0000_1489;

/// The real photo every photo row carries.
pub const PHOTO: &[u8] = include_bytes!("../fixtures/real-photo.jpg");
/// The real voice message every voice row carries.
pub const VOICE: &[u8] = include_bytes!("../fixtures/real-voice.ogg");

/// The bars still red, by name ([`bar`]). A plan step that turns one
/// green takes it out here, or its test fails and says so.
pub const KNOWN_REDS: &[&str] = &[];

/// Whether this build counts; a release build without loquax's
/// `counters` feature counts nothing (`loquax::ui::counters`), and a bar
/// there says `NOT COUNTED` and is not judged, never green.
pub fn counting() -> bool {
    if !loquax::ui::counters::ON {
        println!("NOT COUNTED: a release build without loquax's counters feature");
    }
    loquax::ui::counters::ON
}

/// One bar of the register: `ok` is whether the count stayed within it.
/// A red bar the register names passes and says `KNOWN RED`; a red one it
/// does not name fails; a green one it names fails too, so the step that
/// fixed it takes it out of [`KNOWN_REDS`] in the same commit.
pub fn bar(name: &str, ok: bool, what: &str) {
    judged(KNOWN_REDS, name, ok, what);
}

/// [`bar`] against the register `known_reds`: the register's own test
/// judges with one it holds a name in, which [`KNOWN_REDS`] need not
/// (1600: empty since 970e4f04, and the test that read its first entry
/// panicked on the index).
pub fn judged(known_reds: &[&str], name: &str, ok: bool, what: &str) {
    let known = known_reds.contains(&name);
    match (ok, known) {
        (true, false) => println!("BAR {name} green: {what}"),
        (false, true) => println!("BAR {name} KNOWN RED: {what}"),
        (false, false) => panic!("BAR {name} red: {what}"),
        (true, true) => panic!(
            "BAR {name} is green now: take it out of KNOWN_REDS in \
             loquax/tests/support/real_store.rs ({what})"
        ),
    }
}

/// How long a loop is left alone in the idle bars (1491).
pub const IDLE_MS: u64 = 5_000;
/// The shells' engine pump and clock tick (`loquax/src/shell.rs`
/// `PUMP_MS`, `TICK_MS`).
pub const PUMP_MS: u64 = 20;
pub const TICK_MS: u64 = 250;

/// [`IDLE_MS`] of a loop left alone, folded as the shells fold it
/// (`loquax/src/shell.rs` `poll` and `tick`): the engine's pump every
/// [`PUMP_MS`] (`Elapsed`), the clock every [`TICK_MS`] (`Clock`, then
/// `Tick`), on seconds of [`now`]'s minute, which the five seconds do not
/// turn. After each fold `pass` is the loop's turn: it asks its gate and
/// builds when told to. `elapsed` is where the model's monotonic clock
/// stands at the start.
pub fn idle<B: leviculum_ui::input::InputBuffer + Default>(
    model: &mut loquax::ui::UiModel<B>,
    elapsed: u64,
    mut pass: impl FnMut(&mut loquax::ui::UiModel<B>),
) {
    use loquax::ui::{update, UiEvent};
    let start = now();
    assert!(
        start.unix_secs.rem_euclid(60) + (IDLE_MS / 1000) as i64 + 1 < 60,
        "the idle seconds turn a minute"
    );
    for at in (PUMP_MS..=IDLE_MS).step_by(PUMP_MS as usize) {
        update(model, UiEvent::Elapsed(elapsed + at));
        if at % TICK_MS == 0 {
            let secs = (at / 1000) as i64;
            update(
                model,
                UiEvent::Clock(WallTime {
                    unix_secs: start.unix_secs + secs,
                    ..start
                }),
            );
            update(model, UiEvent::Tick);
        }
        pass(model);
    }
}

/// A splitmix64 stream for conversation `i`.
pub struct Draw(u64);

impl Draw {
    pub fn of(i: usize, salt: u64) -> Self {
        Self(SEED ^ salt ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub fn id(&mut self) -> [u8; 32] {
        let mut id = [0; 32];
        for chunk in id.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_be_bytes());
        }
        id
    }
}

/// Conversation `i`'s peer: the group's address for [`GROUP`].
pub fn peer(i: usize) -> [u8; 16] {
    address(0x50, i)
}

/// Group member `m`'s address (`m` 0 is the admin, the reader is not one
/// of these).
pub fn member(m: usize) -> [u8; 16] {
    address(0x60, m)
}

fn address(tag: u8, i: usize) -> [u8; 16] {
    let mut draw = Draw::of(i, u64::from(tag));
    let mut out = [0; 16];
    out[..8].copy_from_slice(&draw.next().to_be_bytes());
    out[8..].copy_from_slice(&draw.next().to_be_bytes());
    out[0] = tag;
    out
}

const FIRST: [&str; 24] = [
    "Anna", "Bert", "Cem", "Dörte", "Emil", "Fatma", "Gustav", "Hanne", "Ivo", "Jule", "Kai",
    "Lotte", "Mehmet", "Nina", "Ole", "Paula", "Quirin", "Rosa", "Sven", "Tilda", "Uwe", "Vera",
    "Wim", "Zoë",
];

/// The name conversation `i`'s peer announced.
pub fn name(i: usize) -> String {
    match i {
        GROUP => "Hausprojekt Nordstadt".to_string(),
        ANNA => "Anna".to_string(),
        _ => format!("{} {}", FIRST[i % FIRST.len()], i),
    }
}

/// Group member `m`'s name.
pub fn member_name(m: usize) -> String {
    format!("{} M{m}", FIRST[(m * 7) % FIRST.len()])
}

/// How many messages conversation `i` holds.
pub fn messages_in(i: usize) -> usize {
    match i {
        GROUP => GROUP_MESSAGES,
        ANNA => ANNA_MESSAGES,
        i if REPORTERS.contains(&i) => REPORTER_MESSAGES,
        i if FRIENDS.contains(&i) => FRIEND_MESSAGES,
        // The rest share what is left: 1260 over 275, the first 160 five.
        i if i < FRIENDS.end + 160 => 5,
        _ => 4,
    }
}

/// How many of conversation `i`'s newest messages are unread.
pub fn unread_in(i: usize) -> usize {
    match i {
        GROUP => 7,
        ANNA => 3,
        5 | 7 | 8 | 9 | 10 => 2,
        _ => 0,
    }
}

/// How far back conversation `i` reaches, in seconds.
fn span(i: usize) -> i64 {
    match i {
        ANNA => 365 * 86_400,
        GROUP => 90 * 86_400,
        i if REPORTERS.contains(&i) => 4 * 86_400,
        i if FRIENDS.contains(&i) => 60 * 86_400,
        _ => 200 * 86_400,
    }
}

/// When conversation `i`'s newest row lies: the long chat and the group
/// newest, the rest one after the other an hour apart, so the list's
/// order is the index's.
fn newest(i: usize) -> i64 {
    BASE - (i as i64) * 3_600
}

/// The words of the bodies, cut to length.
const WORDS: &str = "Ich habe heute morgen noch mal mit dem Vermieter gesprochen, er meint die \
Heizung im Treppenhaus wird erst nächste Woche repariert, weil der Monteur krank ist. Kannst du \
am Samstag beim Umzug helfen? Wir bräuchten noch zwei Leute für die Waschmaschine und das Sofa, \
das Auto haben wir schon. Die Antenne auf dem Dach läuft übrigens wieder, ich habe das Kabel \
getauscht und jetzt kommen die Pakete aus Köln mit -112 dBm an, das ist besser als vorher 🙂 \
Treffen wir uns um sieben am Bahnhof oder lieber direkt beim Café an der Ecke? Ich bringe den \
Kuchen mit, der Rest ist ja schon organisiert. Gute Nacht und bis morgen! ";

/// A body of `chars` characters, starting at a drawn place.
fn body(draw: &mut Draw, chars: usize) -> String {
    let start = draw.below(WORDS.chars().count() as u64) as usize;
    WORDS.chars().cycle().skip(start).take(chars).collect()
}

/// A position report's telemetry at `at`, `lat_e6` north.
fn position(at: i64, lat_e6: i32, lon_e6: i32) -> Vec<Field> {
    use leviculum_lxmf::telemetry::{Location, Telemetry};
    let telemetry = Telemetry {
        time: Some(at),
        location: Some(Location {
            latitude_e6: lat_e6,
            longitude_e6: lon_e6,
            altitude_e2: 5_120,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 500,
            last_update: at,
        }),
        ..Telemetry::default()
    };
    vec![(
        leviculum_lxmf::constants::FIELD_TELEMETRY,
        telemetry.encode_field_value(),
    )]
}

/// The group's membership as the store keeps it.
pub fn group_info() -> GroupInfo {
    let mut members: Vec<GroupMember> = (0..GROUP_MEMBERS - 1)
        .map(|m| GroupMember {
            address: member(m),
            name: member_name(m),
            admin: m == 0,
            joined: true,
            left: false,
        })
        .collect();
    members.push(GroupMember {
        address: own(),
        name: "Lew".to_string(),
        admin: false,
        joined: true,
        left: false,
    });
    GroupInfo {
        address: peer(GROUP),
        name: name(GROUP),
        admin_name: member_name(0),
        inviter: Some(member(0)),
        ours: false,
        epoch: 3,
        members,
        standing: Standing::Joined,
    }
}

/// The reader's address.
pub fn own() -> [u8; 16] {
    address(0x30, 0)
}

/// Conversation `i`'s messages, oldest first, their keys from `seq` on.
pub fn messages(i: usize, seq: &mut i64) -> Vec<Message> {
    let n = messages_in(i);
    let unread = unread_in(i);
    let mut draw = Draw::of(i, 0x51);
    let step = span(i) / n as i64;
    let mut out: Vec<Message> = Vec::with_capacity(n);
    for j in 0..n {
        let at = newest(i) - (n - 1 - j) as i64 * step;
        // The unread are the newest and all received.
        let outbound = match j + unread >= n {
            true => false,
            // A group is mostly the others' copies, a chat two fifths ours.
            false if i == GROUP => draw.below(10) == 0,
            false => draw.below(5) < 2,
        };
        let chars = match j % 23 {
            11 => 2_000 + draw.below(2_000) as usize,
            _ => 110 + draw.below(220) as usize,
        };
        let mut text = body(&mut draw, chars);
        let mut fields: Vec<Field> = Vec::new();
        let media = i == ANNA || i == GROUP;
        if REPORTERS.contains(&i) && j % 5 != 4 && j < REPORTS * 5 / 4 {
            // A report every half hour, a few words between.
            let lat = 48_137_000 + (i as i32) * 1_000 + (j as i32) * 37;
            fields = position(at, lat, 11_575_000 + (j as i32) * 53);
            text = String::new();
        } else if media && j % 50 == 25 {
            fields = loquax::ui::photos::photo_fields("jpg", PHOTO.to_vec());
            text = String::new();
        } else if media && j % 75 == 40 {
            fields =
                loquax::ui::voice::voice_fields(loquax::ui::voice::AM_OPUS_OGG, VOICE.to_vec());
            text = String::new();
        } else if j >= 2 && j % 13 == 6 {
            let target = out[j - 2].id.unwrap_or([0; 32]);
            fields = vec![(
                leviculum_lxmf::constants::FIELD_REACTION,
                leviculum_lxmf::reaction::Reaction {
                    to: target,
                    content: "\u{1F44D}".to_string(),
                }
                .encode_field_value(),
            )];
            text = "Reacted to your message with \u{1F44D}".to_string();
        } else if j >= 1 && j % 9 == 4 {
            let target = &out[j - 1];
            fields = leviculum_lxmf::reply::Reply {
                to: target.id.unwrap_or([0; 32]),
                quote: Some(loquax::ui::replies::quote(&target.body)),
            }
            .fields()
            .to_vec();
        }
        if i == GROUP && !outbound {
            // A copy: its writer, the home's "Name: " before the text.
            let m = 1 + draw.below(GROUP_MEMBERS as u64 - 2) as usize;
            let copy = Lev {
                capabilities: Some(CAP_GROUPS),
                group_copy: Some(GroupCopy {
                    sender: member(m),
                    sent_ms: (at as u64) * 1_000 + j as u64,
                    epoch: 3,
                }),
                ..Lev::default()
            };
            fields.extend(copy.fields());
            text = format!("{}: {text}", member_name(m));
        }
        let delivery = outbound.then(|| match j % 7 {
            0 => DeliveryState::Read {
                at: u32::try_from(at + 300).unwrap_or(0),
            },
            _ => DeliveryState::Delivered,
        });
        let key = loquax::store::key_of(*seq);
        *seq += 1;
        out.push(Message {
            outbound,
            size: text.len() + fields.iter().map(|(_, v)| v.len()).sum::<usize>(),
            body: text,
            id: Some(draw.id()),
            delivery,
            signature: Signature::Verified,
            leg: None,
            unproven: false,
            at: Some(WallTime {
                unix_secs: at,
                offset_secs: 3_600,
            }),
            ledger: Vec::new(),
            last_leg: None,
            activity: key,
            fields,
        });
    }
    out
}

/// The conversation's flags.
fn flags(i: usize) -> Flags {
    Flags {
        muted_until: (i == MUTED).then_some(i64::MAX),
        archived: ARCHIVED.contains(&i),
        verified: false,
        receipts: false,
    }
}

/// Every conversation as the model takes it, every message in it: a
/// device after its first pull, before a restart.
pub fn restored() -> Vec<Restored> {
    let mut seq = 1;
    (0..CONVERSATIONS)
        .map(|i| Restored {
            peer: peer(i),
            pinned: i == ANNA,
            flags: flags(i),
            unread: unread_in(i),
            messages: messages(i, &mut seq),
            earlier: 0,
            draft: Draft::default(),
            group: (i == GROUP).then(group_info),
            listing: None,
        })
        .collect()
}

/// Every peer's announced name, the members' too.
pub fn names() -> Vec<([u8; 16], String)> {
    (0..CONVERSATIONS)
        .map(|i| (peer(i), name(i)))
        .chain((0..GROUP_MEMBERS - 1).map(|m| (member(m), member_name(m))))
        .collect()
}

/// A day's hour after the newest row: every time on screen is fixed.
pub fn now() -> WallTime {
    WallTime {
        unix_secs: BASE + 3_600,
        offset_secs: 3_600,
    }
}

/// The model of a device that holds the fixture in memory, as after its
/// first pull: the names, the clock, every message, the reader's address.
pub fn model<B: leviculum_ui::input::InputBuffer + Default>() -> loquax::ui::UiModel<B> {
    let mut model = loquax::ui::UiModel::<B>::new(&[]);
    model.address = Some(own());
    for (peer, name) in names() {
        model.names.remember(peer, &name);
    }
    loquax::ui::update(&mut model, loquax::ui::UiEvent::Clock(now()));
    model.restore(restored());
    model
}

/// The rows of [`restored`] written into the store at `home` for the
/// reader's address, as the screen writes them: each conversation's
/// messages, its state and each report's position in one write, then the
/// names. What a restart restores from.
pub fn fill_store(home: &Path) -> Store {
    fill_store_for(home, own())
}

/// [`fill_store`] for the address `me`: a phone's client store, whose
/// address is its mailbox's.
pub fn fill_store_for(home: &Path, me: [u8; 16]) -> Store {
    let mut store = Store::open_in(home, me).expect("the store");
    // The rows the list shows (1490, schema 17), derived as the fold
    // derives them: written beside each conversation, as take_writes
    // writes them, so a restart reads summaries and no body.
    let shown: loquax::ui::UiModel<leviculum_ui::input::Line> = model();
    for stored in restored() {
        let mut writes = Vec::new();
        if let Some(group) = &stored.group {
            writes.push(StoreWrite::Group(group.clone()));
        }
        for message in &stored.messages {
            if !message.outbound {
                let read = loquax::telemetry::read(
                    stored.peer,
                    &message.fields,
                    message.at.map(|at| at.unix_secs),
                );
                writes.extend(read.fixes.into_iter().map(|fix| {
                    StoreWrite::Position(loquax::telemetry::Fix {
                        message: Some(message.activity),
                        ..fix
                    })
                }));
            }
            writes.push(StoreWrite::Message {
                peer: stored.peer,
                message: message.clone(),
            });
        }
        writes.push(StoreWrite::Conversation {
            peer: stored.peer,
            pinned: stored.pinned,
            unread: stored.unread,
            flags: stored.flags,
        });
        writes.push(StoreWrite::Listing {
            peer: stored.peer,
            listing: shown.listing_for(&stored.peer).expect("a filled row"),
        });
        store.write(&writes).expect("a conversation");
    }
    for (peer, name) in names() {
        store.save_name(peer, &name).expect("a name");
    }
    store
}

/// The model a restart builds from the store at `home`, as the shell
/// does (`loquax/src/shell.rs`, `start`): the names, the conversations,
/// the positions.
pub fn model_from_store<B: leviculum_ui::input::InputBuffer + Default>(
    store: &Store,
) -> loquax::ui::UiModel<B> {
    let mut model = loquax::ui::UiModel::<B>::new(&[]);
    model.address = Some(own());
    for (peer, name) in store.names().expect("names") {
        model.names.remember(peer, &name);
    }
    loquax::ui::update(&mut model, loquax::ui::UiEvent::Clock(now()));
    model.restore(store.restore().expect("restore"));
    model.restore_positions(store.positions().expect("positions"));
    model
}

/// The fixture is what its table says: 300 conversations, 4000 messages,
/// the group's members and copies, the photos, voices, replies, reactions,
/// reports, the unread; the counts exact, since every row is drawn from
/// [`SEED`]. Called by each target's test that uses it, so a
/// change to the shape is seen where it is used.
pub fn check_shape() {
    let restored = restored();
    assert_eq!(restored.len(), CONVERSATIONS);
    let all: Vec<&Message> = restored.iter().flat_map(|r| r.messages.iter()).collect();
    assert_eq!(all.len(), MESSAGES);
    assert_eq!(restored[GROUP].messages.len(), GROUP_MESSAGES);
    assert_eq!(
        restored[GROUP].group.as_ref().map(|g| g.members.len()),
        Some(GROUP_MEMBERS)
    );
    assert_eq!(restored[ANNA].messages.len(), ANNA_MESSAGES);
    assert_eq!(restored.iter().map(|r| r.unread).sum::<usize>(), UNREAD);
    let has = |key: i64| {
        all.iter()
            .filter(|m| m.fields.iter().any(|(k, _)| *k == key))
            .count()
    };
    use leviculum_lxmf::constants::{FIELD_AUDIO, FIELD_IMAGE, FIELD_REACTION, FIELD_TELEMETRY};
    let replies = all
        .iter()
        .filter(|m| leviculum_lxmf::reply::Reply::from_fields(&m.fields).is_some())
        .count();
    let long = all
        .iter()
        .filter(|m| m.body.chars().count() >= 2_000)
        .count();
    let copies = restored[GROUP]
        .messages
        .iter()
        .filter(|m| Lev::from_fields(&m.fields).is_some_and(|l| l.group_copy.is_some()))
        .count();
    let shape = [
        has(FIELD_IMAGE),
        has(FIELD_AUDIO),
        has(FIELD_REACTION),
        has(FIELD_TELEMETRY),
        replies,
        long,
        copies,
    ];
    println!(
        "SHAPE photos={} voices={} reactions={} reports={} replies={} long={} copies={}",
        shape[0], shape[1], shape[2], shape[3], shape[4], shape[5], shape[6]
    );
    assert_eq!(shape, [34, 23, 189, 192, 410, 109, 456]);
}
