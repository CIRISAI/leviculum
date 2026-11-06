//! A store shaped like a real one after a year of use and two imports
//! (1173), for the tests whose own small, fresh, well formed stores let
//! three device bugs of 2026-10-30/31 through: an import the running
//! session did not see, names that did not come, and Info that failed on an
//! imported row's negative key. Shared by `#[path]` as `nomadnet_tree.rs`
//! is: `tui_snapshot.rs`, `loquax-android/tests/facade.rs` and `map.rs`.
//!
//! Two writers fill the same rows:
//!
//! - [`fill_archive`] into a home node's archive, the way the mailbox holds
//!   them: imported rows as [`loquax::home::KIND_IMPORTED`] entries, live
//!   ones as signed `KIND_RECEIVED` and `KIND_SENT` messages with their
//!   state entries, names, petnames, peer states and read marks. A phone
//!   that joins pulls all of it, and its store lays the imported block out
//!   below seq 1, so those rows carry negative keys on the Java side.
//! - [`fill_store`] into a loquax's own store and contacts file, the way a
//!   terminal loquax that imported and then talked holds them.
//!
//! Everything is a function of [`SEED`] and the conversation's index: the
//! same bytes on every run.
//!
//! | conv  | peer seed   | source            | rows | name                         | state                    |
//! |-------|-------------|-------------------|------|------------------------------|--------------------------|
//! | 0     | `0x40`      | Ratspeak, v1      | 40   | petname `Ratte Größe 🐀`     |                          |
//! | 1     | `0x41`      | NomadNet, v2      | 40   | petname `Nomadin Ünal`       |                          |
//! | 2     | `0x42`      | live              | 40   | announced `Jürgen 🚲`        | pinned                   |
//! | 3     | `0x43`      | live              | 40   | announced `Zoë 💬 Ærø`       |                          |
//! | 4     | `0x44`      | live              | 40   | petname `Mum`, announced `x` | muted always             |
//! | 5..17 | `0x45..`    | live              | 40   | announced `Peer n` on odd n  |                          |
//! | 18    | `0x52`      | live              | 40   | none: the address            | archived                 |
//! | 19    | `0x53`      | live              | 40   | announced `Spam`             | blocked                  |
//!
//! 800 messages. Row `j` of conversation `i` lies at
//! `BASE + j * 600 + i * 7` (imports a year earlier), alternating received
//! (`j` even) and sent (`j` odd). In every conversation:
//!
//! | row `j`        | what                                                       |
//! |----------------|------------------------------------------------------------|
//! | 3              | a body of [`LONG_CHARS`] characters, umlauts and emoji     |
//! | 7, 21, 34      | a reply to row `j - 1`, quoting it                         |
//! | 11, 24         | a reaction to row `j - 2` (Ratspeak's words in its body)   |
//! | 14             | a received position with a body                            |
//! | 18             | a received empty body with a position only                 |
//! | 29             | an empty body, an empty title, no fields                   |
//! | sent, by `j`   | `j % 8`: 1 delivered, 3 failed, 5 in a mailbox, 7 queued   |
//! | 15, 31 imported | read ten minutes later (1232): every rung in the chat |
//! | others         | [`BODIES`] in turn                                         |
//!
//! The NomadNet rows carry provenance: received rows alternate verified,
//! held for an unknown key (the signed bytes kept), and one invalid (row
//! 9); the source's own word on the signature is kept where it gave one.
//! The archive also holds a read mark for the received rows of
//! conversations 2 to 9.

#![allow(dead_code)]

use std::path::Path;

use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm};
use leviculum_lxmf::Field;
use leviculum_std::Identity;
use leviculum_ui::delivery::DeliveryState;
use loquax::home::wire;
use loquax::store::{ImportRow, Store};
use loquax::ui::Signature;

/// What every choice below is drawn from.
pub const SEED: u64 = 0x1173_0000_0000_1173;
/// How many conversations the store holds.
pub const CONVERSATIONS: usize = 20;
/// How many messages each holds.
pub const PER_CONVERSATION: usize = 40;
/// The conversation imported from Ratspeak (entry version 1).
pub const RATSPEAK: usize = 0;
/// The conversation imported from NomadNet (entry version 2).
pub const NOMADNET: usize = 1;
/// The pinned conversation.
pub const PINNED: usize = 2;
/// The conversation muted for always.
pub const MUTED: usize = 4;
/// The archived conversation.
pub const ARCHIVED: usize = 18;
/// The blocked peer's conversation.
pub const BLOCKED: usize = 19;
/// How many of a conversation's rows are reactions, which a screen folds
/// onto their targets: it shows `PER_CONVERSATION - REACTIONS` bubbles.
pub const REACTIONS: usize = 2;
/// The long body's length in characters.
pub const LONG_CHARS: usize = 4_000;
/// The newest live row lies shortly after this: 2026-05-28.
pub const BASE: i64 = 1_780_000_000;
/// How much earlier the imported conversations lie.
pub const IMPORT_EARLIER: i64 = 365 * 86_400;
/// The archive cap the writers use: the home node's of the facade tests.
pub const CAP: u64 = 1 << 20;

/// The bodies of the ordinary rows, in turn.
pub const BODIES: [&str; 8] = [
    "Grüße aus Köln, die Straße ist nass",
    "ok 👍",
    "Treffen wir uns um 18:00 am Bahnhof?",
    "naïve façade, smörgåsbord, Ærøskøbing",
    "日本語のテキストも",
    "😀🐀🚲 drei Emoji am Stück",
    "zwei\nZeilen",
    "ja",
];

/// The petname of conversation `i`, where it has one.
pub fn petname(i: usize) -> Option<&'static str> {
    match i {
        RATSPEAK => Some("Ratte Größe 🐀"),
        NOMADNET => Some("Nomadin Ünal"),
        MUTED => Some("Mum"),
        _ => None,
    }
}

/// The name conversation `i`'s peer announced, where it did.
pub fn announced(i: usize) -> Option<String> {
    match i {
        2 => Some("Jürgen 🚲".to_string()),
        3 => Some("Zoë 💬 Ærø".to_string()),
        MUTED => Some("x".to_string()),
        ARCHIVED => None,
        BLOCKED => Some("Spam".to_string()),
        n if n >= 5 && n % 2 == 1 => Some(format!("Peer {n}")),
        _ => None,
    }
}

/// The name a screen shows first for conversation `i`: its petname, else
/// what it announced; `None` for an address only.
pub fn shown_name(i: usize) -> Option<String> {
    petname(i).map(str::to_string).or_else(|| announced(i))
}

/// Conversation `i`'s peer.
pub fn identity(i: usize) -> Identity {
    Identity::from_private_key_bytes(&[0x40 + i as u8; 64]).expect("a 64-byte key")
}

/// Conversation `i`'s peer's address.
pub fn peer(i: usize) -> [u8; 16] {
    loquax::engine::delivery_address(&identity(i)).expect("an address")
}

/// Conversation `i`'s peer's address in hex, as the facade names it.
pub fn peer_hex(i: usize) -> String {
    loquax::address::to_hex(&peer(i))
}

/// A splitmix64 stream: the ids of the rows no signed message names.
struct Draw(u64);

impl Draw {
    fn of(i: usize) -> Self {
        Self(SEED ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn id(&mut self) -> [u8; 32] {
        let mut id = [0; 32];
        for chunk in id.chunks_mut(8) {
            chunk.copy_from_slice(&self.next().to_be_bytes());
        }
        id
    }
}

/// One message of the fixture, whichever writer takes it.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: [u8; 32],
    pub outbound: bool,
    /// Unix seconds, the sender's clock and the receive time alike.
    pub at: i64,
    pub body: String,
    pub fields: Vec<Field>,
    /// A sent row's state; `None` for a received one.
    pub delivery: Option<DeliveryState>,
    pub signature: Signature,
    pub origin_verdict: Option<bool>,
    pub held: Option<([u8; 16], Vec<u8>)>,
    /// The signed message, for a live row and a NomadNet one.
    pub packed: Option<Vec<u8>>,
}

/// A conversation beside the twenty, for the fold of 1184: a text, a run
/// of [`RUN_REPORTS`] position reports a minute apart, a text, all unread
/// ([`fill_run_of`]).
pub const RUN: usize = CONVERSATIONS;
/// The reports of [`RUN`]'s run, as Lew's phone got them from Columba in
/// twelve minutes on 2026-10-31.
pub const RUN_REPORTS: usize = 14;

/// The [`LONG_CHARS`] character body: umlauts and emoji, so a cut in the
/// middle of a character would show.
pub fn long_body() -> String {
    "Grüße 🐀 ".chars().cycle().take(LONG_CHARS).collect()
}

/// A position report's field, as Sideband and Columba send it.
pub fn position(at: i64, lat_e6: i32) -> Vec<Field> {
    use leviculum_lxmf::telemetry::{Location, Telemetry};
    let telemetry = Telemetry {
        time: Some(at),
        location: Some(Location {
            latitude_e6: lat_e6,
            longitude_e6: 11_256_000,
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

fn reaction(to: [u8; 32], content: &str) -> Vec<Field> {
    let reaction = leviculum_lxmf::reaction::Reaction {
        to,
        content: content.to_string(),
    };
    vec![(
        leviculum_lxmf::constants::FIELD_REACTION,
        reaction.encode_field_value(),
    )]
}

/// Row `j`'s state, sent at `at` in conversation `i`: an imported
/// conversation (only the import carries read) holds every rung of the
/// ladder, the device walk's chat among them.
fn delivery_of(i: usize, j: usize, at: i64) -> DeliveryState {
    let imported = i == RATSPEAK || i == NOMADNET;
    if imported && j % 16 == 15 {
        return DeliveryState::Read {
            at: u32::try_from(at + 600).unwrap_or(0),
        };
    }
    match j % 8 {
        1 => DeliveryState::Delivered,
        3 => DeliveryState::Failed,
        5 => DeliveryState::InMailbox,
        _ => DeliveryState::Queued,
    }
}

/// The rows of conversation `i`, oldest first, for the home identity `own`.
pub fn rows(own: &Identity, i: usize) -> Vec<Row> {
    let me = loquax::engine::delivery_address(own).expect("our address");
    let them = identity(i);
    let peer = peer(i);
    let mut draw = Draw::of(i);
    let earlier = if i == RATSPEAK || i == NOMADNET {
        IMPORT_EARLIER
    } else {
        0
    };
    let mut rows: Vec<Row> = Vec::with_capacity(PER_CONVERSATION);
    for j in 0..PER_CONVERSATION {
        let outbound = j % 2 == 1;
        let at = BASE - earlier + (j as i64) * 600 + (i as i64) * 7;
        let mut body = BODIES[(i + j) % BODIES.len()].to_string();
        let mut fields = Vec::new();
        match j {
            3 => body = long_body(),
            7 | 21 | 34 => {
                let target = &rows[j - 1];
                fields = leviculum_lxmf::reply::Reply {
                    to: target.id,
                    quote: Some(loquax::ui::replies::quote(&target.body)),
                }
                .fields();
                body = format!("Antwort {j}: genau, äh, so 🙂");
            }
            11 | 24 => {
                fields = reaction(rows[j - 2].id, "\u{1F44D}");
                body = "Reacted to your message with \u{1F44D}".to_string();
            }
            14 => {
                fields = position(at, 48_137_000 + i as i32 * 1_000);
                body = "hier bin ich".to_string();
            }
            18 => {
                fields = position(at, 48_100_000 + i as i32 * 1_000);
                body = String::new();
            }
            29 => body = String::new(),
            _ => {}
        }
        let delivery = outbound.then(|| delivery_of(i, j, at));
        let packed = (i != RATSPEAK).then(|| {
            let (to, from, signer) = match outbound {
                true => (peer, me, own),
                false => (me, peer, &them),
            };
            Lxm::create(
                to,
                from,
                signer,
                at as f64,
                Vec::new(),
                body.as_bytes().to_vec(),
                fields.clone(),
                DeliveryMethod::Direct,
            )
            .expect("a message")
        });
        let id = match &packed {
            Some(message) => message.message_id,
            None => draw.id(),
        };
        let packed = packed.map(|message| message.pack());
        let (signature, origin_verdict, held) = match (i, outbound) {
            (NOMADNET, true) => (Signature::Verified, Some(true), None),
            (NOMADNET, false) if j == 9 => (Signature::Invalid, Some(false), None),
            (NOMADNET, false) if j % 4 == 0 => (Signature::Verified, Some(true), None),
            (NOMADNET, false) => (
                Signature::Unverified,
                None,
                Some((peer, packed.clone().expect("signed"))),
            ),
            _ => (Signature::Unverified, None, None),
        };
        rows.push(Row {
            id,
            outbound,
            at,
            body,
            fields,
            delivery,
            signature,
            origin_verdict,
            held,
            packed,
        });
    }
    rows
}

impl Row {
    /// The row as an import keeps it.
    pub fn import_row(&self) -> ImportRow {
        ImportRow {
            id: self.id,
            outbound: self.outbound,
            timestamp: Some(self.at as f64),
            received_at: Some(self.at),
            title: Vec::new(),
            content: self.body.clone(),
            fields: self.fields.clone(),
            delivery: self.delivery,
            signature: self.signature,
            origin_verdict: self.origin_verdict,
            held: self.held.clone(),
        }
    }
}

/// The origin tag an import writes for conversation `i`.
fn origin(i: usize) -> &'static str {
    match i {
        RATSPEAK => "ratspeak",
        _ => "nomadnet",
    }
}

/// The peer state conversation `i` carries, if any.
fn peer_state(i: usize) -> Option<wire::PeerState> {
    let mut state = wire::PeerState::default();
    match i {
        PINNED => state.pinned = true,
        MUTED => state.muted_until = Some(i64::MAX),
        ARCHIVED => state.archived = true,
        BLOCKED => state.blocked = true,
        _ => return None,
    }
    Some(state)
}

/// What a writer put in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Filled {
    pub messages: usize,
    pub imported: usize,
}

/// Fill the archive of the home node in `home` (its `identity` file made
/// if missing), as the mailbox holds a year of use and two imports.
pub fn fill_archive(home: &Path) -> Filled {
    fill_archive_of(home, CONVERSATIONS, CAP)
}

/// The most conversations [`fill_archive_of`] can draw: a peer's key byte
/// is `0x40 + i`.
pub const MAX_CONVERSATIONS: usize = 0x100 - 0x40;

/// [`fill_archive`] with `conversations` conversations of
/// [`PER_CONVERSATION`] rows each, those past the twenty live, without a
/// state, named as [`announced`] says, under an archive cap of `cap` bytes: a mailbox as
/// big as Lew's, thousands of entries, for the first pull of 1446.
pub fn fill_archive_of(home: &Path, conversations: usize, cap: u64) -> Filled {
    assert!(
        conversations <= MAX_CONVERSATIONS,
        "{conversations} conversations"
    );
    let own = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
    let me = loquax::engine::delivery_address(&own).expect("address");
    let mut store = Store::open_in(home, me).expect("the home store");
    let mut filled = Filled::default();
    for i in 0..conversations {
        let peer = peer(i);
        let rows = rows(&own, i);
        if i == RATSPEAK || i == NOMADNET {
            let entries: Vec<([u8; 32], i64, Vec<u8>)> = rows
                .iter()
                .map(|row| {
                    let entry = loquax::import::archive::entry(origin(i), &peer, &row.import_row());
                    (row.id, row.at, entry)
                })
                .collect();
            let added = store.archive_imported(&entries, cap).expect("archive");
            assert_eq!(added, rows.len());
            filled.imported += added;
            filled.messages += added;
            continue;
        }
        for row in &rows {
            let packed = row.packed.as_ref().expect("a live row is signed");
            let kind = match row.outbound {
                true => loquax::home::KIND_SENT,
                false => loquax::home::KIND_RECEIVED,
            };
            store
                .archive_append(kind, &row.id, packed, row.at, cap)
                .expect("append")
                .expect("new");
            filled.messages += 1;
            let state = match row.delivery {
                Some(DeliveryState::Delivered) => Some(loquax::home::KIND_DELIVERED),
                Some(DeliveryState::Failed) => Some(loquax::home::KIND_FAILED),
                Some(DeliveryState::InMailbox) => Some(loquax::home::KIND_PROPAGATED),
                _ => None,
            };
            if let Some(kind) = state {
                store
                    .archive_append(kind, &row.id, &row.id, row.at + 30, cap)
                    .expect("a state");
            }
        }
        if (2..10).contains(&i) {
            let ids: Vec<[u8; 32]> = rows.iter().filter(|r| !r.outbound).map(|r| r.id).collect();
            let mark = wire::Mark {
                flag: wire::MarkFlag::Read,
                ids,
            };
            let at = rows.last().map_or(BASE, |r| r.at) + 60;
            store
                .archive_append(
                    loquax::home::KIND_MARK,
                    &rows[0].id,
                    &wire::mark_request(&mark),
                    at,
                    cap,
                )
                .expect("a mark");
        }
    }
    let at = BASE + 86_400;
    for i in 0..conversations {
        if let Some(name) = announced(i) {
            store.archive_name(peer(i), &name, at, cap).expect("a name");
        }
        if let Some(state) = peer_state(i) {
            store
                .archive_peer_state(peer(i), &wire::peer_state_entry(&peer(i), &state), at, cap)
                .expect("a peer state");
        }
    }
    let peers: Vec<([u8; 16], &str)> = (0..conversations)
        .filter_map(|i| petname(i).map(|name| (peer(i), name)))
        .collect();
    let named = loquax::home::server::archive_petnames(
        &mut store,
        peers.iter().map(|(peer, name)| (peer, *name)),
        at,
        cap,
    )
    .expect("the petnames");
    assert_eq!(named, peers.len());
    filled
}

/// Fill the store and contacts file of the loquax in `home` (its
/// `identity` file made if missing), as a terminal loquax holds the same
/// year: the imports through the import's own write, the live rows as the
/// screen writes them, each received position with its message.
pub fn fill_store(home: &Path) -> Filled {
    let own = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
    fill_store_of(home, &own)
}

/// [`fill_store`] for the address of `own`, whatever `home` holds: a
/// phone's client store, whose address is the mailbox's.
pub fn fill_store_of(home: &Path, own: &Identity) -> Filled {
    use loquax::ui::history::{Flags, StoreWrite};
    use loquax::when::WallTime;
    let me = loquax::engine::delivery_address(own).expect("address");
    let mut store = Store::open_in(home, me).expect("the store");
    let mut filled = Filled::default();
    let mut seq = 1;
    for i in 0..CONVERSATIONS {
        let peer = peer(i);
        let rows = rows(own, i);
        if i == RATSPEAK || i == NOMADNET {
            let import: Vec<ImportRow> = rows.iter().map(Row::import_row).collect();
            let counts = store
                .import_conversation(peer, origin(i), &import)
                .expect("import");
            assert_eq!(counts.new, rows.len());
            filled.imported += counts.new;
            filled.messages += counts.new;
            continue;
        }
        let mut writes = Vec::new();
        for row in &rows {
            let at = Some(WallTime {
                unix_secs: row.at,
                offset_secs: 0,
            });
            let key = loquax::store::key_of(seq);
            writes.push(StoreWrite::Message {
                peer,
                message: loquax::ui::Message {
                    outbound: row.outbound,
                    body: row.body.clone(),
                    id: Some(row.id),
                    delivery: row.delivery,
                    signature: match row.outbound {
                        true => Signature::Verified,
                        false => Signature::Unverified,
                    },
                    leg: None,
                    unproven: false,
                    at,
                    size: row.body.len(),
                    ledger: Vec::new(),
                    last_leg: None,
                    activity: key,
                    fields: row.fields.clone(),
                },
            });
            if !row.outbound {
                let read = loquax::telemetry::read(peer, &row.fields, Some(row.at));
                writes.extend(read.fixes.into_iter().map(|fix| {
                    StoreWrite::Position(loquax::telemetry::Fix {
                        message: Some(key),
                        ..fix
                    })
                }));
            }
            seq += 1;
            filled.messages += 1;
        }
        let state = peer_state(i).unwrap_or_default();
        writes.push(StoreWrite::Conversation {
            peer,
            pinned: state.pinned,
            unread: usize::from(i >= 10) * 3,
            flags: Flags {
                muted_until: state.muted_until,
                archived: state.archived,
                verified: false,
                receipts: false,
            },
        });
        store.write(&writes).expect("the live rows");
    }
    for i in 0..CONVERSATIONS {
        if let Some(name) = announced(i) {
            store.save_name(peer(i), &name).expect("a name");
        }
        if petname(i).is_some() || i == BLOCKED {
            loquax::names::save_contact(
                home,
                &loquax::names::Contact {
                    peer: peer(i),
                    petname: petname(i).map(str::to_string),
                    blocked: i == BLOCKED,
                },
            )
            .expect("the contacts file");
        }
    }
    filled
}

/// Write conversation [`RUN`] into the store of the loquax in `home` for the
/// address of `own`, after [`fill_store_of`]: `losgefahren`, then
/// [`RUN_REPORTS`] position reports a minute apart, then `angekommen`,
/// every one received and unread. Their keys lie above the ugly store's.
pub fn fill_run_of(home: &Path, own: &Identity) -> Filled {
    use loquax::ui::history::{Flags, StoreWrite};
    use loquax::when::WallTime;
    let me = loquax::engine::delivery_address(own).expect("address");
    let mut store = Store::open_in(home, me).expect("the store");
    let peer = peer(RUN);
    let start = BASE + 86_400;
    let mut writes = Vec::new();
    let count = RUN_REPORTS + 2;
    for j in 0..count {
        let at = start + j as i64 * 60;
        let (body, fields) = match j {
            0 => ("losgefahren".to_string(), Vec::new()),
            j if j == count - 1 => ("angekommen".to_string(), Vec::new()),
            j => (String::new(), position(at, 52_524_360 + j as i32)),
        };
        let key = loquax::store::key_of(10_000 + j as i64);
        let id = [0x84 ^ j as u8; 32];
        let fixes = loquax::telemetry::read(peer, &fields, Some(at)).fixes;
        writes.push(StoreWrite::Message {
            peer,
            message: loquax::ui::Message {
                outbound: false,
                size: body.len(),
                body,
                id: Some(id),
                delivery: None,
                signature: Signature::Unverified,
                leg: None,
                unproven: false,
                at: Some(WallTime {
                    unix_secs: at,
                    offset_secs: 0,
                }),
                ledger: Vec::new(),
                last_leg: None,
                activity: key,
                fields,
            },
        });
        writes.extend(fixes.into_iter().map(|fix| {
            StoreWrite::Position(loquax::telemetry::Fix {
                message: Some(key),
                ..fix
            })
        }));
    }
    writes.push(StoreWrite::Conversation {
        peer,
        pinned: false,
        unread: count,
        flags: Flags::default(),
    });
    store.write(&writes).expect("the run");
    Filled {
        messages: count,
        imported: 0,
    }
}
