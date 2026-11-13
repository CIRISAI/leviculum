//! The message store: conversations, messages and their ledgers across
//! restarts, in SQLite (`docs/src/concepts/loquax-storage.md`, "Decision
//! (2026-08-10)"), at `${LNMSG_HOME}/history.sqlite3`.
//!
//! The model says what changed ([`crate::ui::history::StoreWrite`]); this
//! file writes it, one transaction a fold, and reads the history back at
//! start ([`Store::restore`]), a page at a time further back
//! ([`Store::earlier`]), and through its FTS5 index ([`Store::search`]).
//!
//! # The schema
//!
//! The page's starting point, with its three commitments: every row is
//! scoped by our own address (`identity`), a message keeps the sender's
//! clock (`timestamp`) apart from ours (`received_at`), and `fields` is raw
//! msgpack. Where it differs:
//!
//! - **The key is the screen's message counter** (`seq`, the model's
//!   [`Message::activity`]), not `(id, identity)`: a message we write has
//!   no LXMF id until the router queued it, and a send that falls back to a
//!   mailbox gets a second one. The id is a column, indexed.
//! - **The ledger is a table of its own**, a row a line, deleted with its
//!   message; the delivery ledger shares the message's retention (the
//!   page's "Retention").
//! - **Conversations are a table** for what is the conversation's and not a
//!   message's: the pin and the unread count. Names heard from announces
//!   are one more, so a peer keeps its name across a restart.
//! - **`fields` is filled, `title` and `timestamp` stay empty for now**:
//!   the engine's `Received` event carries the fields, which the store keeps
//!   whole as one raw msgpack map, and the screen drops the title. The
//!   column `received_at` holds the time a message shows, which since 1445
//!   is when its writer wrote it (the event's `sent_at`, a group copy's
//!   writer's); when this device took it is the first line of its ledger.
//!   The columns are there so filling them is no migration.
//! - **Positions are a table** (`positions`, schema 2): every position a
//!   telemetry field carried, keyed by who measured and the fix time, so the
//!   same fix arriving directly and again in a collector's stream is one row
//!   (`docs/src/concepts/loquax-map.md`, "Stored where"). The wire's
//!   integers go in as they are.
//! - **Unverified messages keep their bytes** (`unverified`, schema 3): a
//!   received message whose sender's key was not known, as it came, so its
//!   signature can be checked when the key arrives, in this run or a later
//!   one (leviculum 1079). The row goes once the verdict is in, which
//!   `messages.verification` then holds: 0 unverified, 1 verified, 2
//!   invalid. A message stored unverified before schema 3 has no bytes kept
//!   and stays unverified: the signature was never stored.
//!
//! - **The home node's archive is a table** (`archive`, schema 4): on a
//!   home node (`docs/src/concepts/loquax-home-node.md`) every received
//!   message is appended as it came, packed, under a sequence that only
//!   grows (`AUTOINCREMENT`, so a sequence is never handed out twice, even
//!   after the newest entry was evicted). Its clients pull it by cursor. The
//!   admitted devices and the open admission tokens are two more tables.
//! - **The home node's outbox is a table** (`home_outbox`, schema 5): every
//!   message a client handed in with `/send`, signed and packed once, with
//!   its state, so a restart keeps retrying it. Each state change is an
//!   archive entry as well, written in the same transaction.
//! - **Drafts are a table** (`drafts`, schema 8, 1219): a conversation's
//!   message typed and not sent, one row a conversation, beside it and not
//!   in the config. The model writes it once typing paused
//!   ([`crate::ui::history::DRAFT_DEBOUNCE_MS`]), each write one
//!   transaction as every write is, and deletes it once sent.
//! - **Read receipts are two columns** (schema 9, 1230): a message of ours
//!   the peer read is state 8 with the reader's time in `messages.read_at`,
//!   and `conversations.receipts` says whether the peer's latest message
//!   carried the receipt mark (`docs/src/concepts/loquax-read-receipts.md`).
//! - **Position shares are a table** (`shares`, schema 10, 1273): a
//!   conversation's running share, one row a conversation: its span, its
//!   start and end in unix seconds (no end until stopped), and the last
//!   report it sent (the fix's integers, when it went, whether it went for
//!   a move), which is what its cadence goes on from after a restart. The
//!   row goes when the share stops.
//! - **Imported history sits below seq 0** (schema 6, `crate::import`): a
//!   conversation's imported rows are older than what loquax wrote, so they
//!   take negative keys below the store's lowest, in the source's time
//!   order, and say where they came from (`origin`) and what the source app
//!   thought of their signature (`origin_verdict`), never as our verdict
//!   (`docs/src/concepts/loquax-import.md`). The model counts its keys
//!   unsigned, so the store hands it `seq` with the sign bit flipped
//!   ([`key_of`]): the order is the same, seq 0 is [`KEY_OF_SEQ_ZERO`].
//!
//! # Versions
//!
//! Schema 1 is the store of slice 6; schema 2 adds `positions`, schema 3
//! `unverified`, schema 4 the home node's three tables, schema 5 its
//! outbox, schema 6 the two provenance columns of imported messages, schema 7
//! a conversation's mute, archive and verification, schema 8 the drafts,
//! schema 9 the two read-receipt columns, schema 10 the shares, schema 11
//! the groups a home hosts, schema 12 a copy's digest and the groups a
//! device knows and schema 13 whether a copy waits at a propagation node,
//! nothing else, so
//! an older file is migrated in place at open, in one transaction. A file newer than this program is refused
//! ([`StoreError::Newer`]): a loquax that knows only schema 1 refuses a
//! schema 2 file, and leaves it as it is.
//!
//! # The durability rule
//!
//! From the page: nothing here may treat a corrupt load as an empty one. A
//! missing file is a first start and makes a fresh store. A file that is
//! there and is not a whole loquax store (not SQLite at all, shorter than
//! its own header says, a schema that is not ours, or newer than this
//! program; SQLite's own checks at open, and the schema's) is refused, with the file named, and loquax does not start over
//! it. Every write is one transaction under SQLite's rollback journal with
//! `synchronous=FULL`, so a crash leaves the store as it was before the
//! write or after it.
//!
//! # Retention
//!
//! As decided: message text and its ledger are kept until the reader
//! deletes them. Attachments, which have the byte budget, do not exist yet.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use leviculum_ui::delivery::DeliveryState;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::home::outgoing::Change;
use crate::send::Leg;
use crate::telemetry::{pack_fields, unpack_fields, Fix};
use crate::ui::counters::{Counters, Op, Ops};
use crate::ui::groups::{GroupInfo, Standing};
use crate::ui::history::{Draft, Flags, Restored, StoreWrite, StoredHit, LOAD_WINDOW};
use crate::ui::listing::Listing;
use crate::ui::share::{Report, Share, Span};
use crate::ui::{LedgerEntry, Message, Signature};
use crate::when::WallTime;
use leviculum_lxmf::telemetry::Location;

/// The store's file under the loquax home directory.
pub const STORE_FILE: &str = "history.sqlite3";

/// The schema this program writes, in SQLite's `user_version`.
pub const SCHEMA_VERSION: i32 = 22;

/// How long a write waits for another loquax holding the file.
const BUSY_MS: u64 = 5_000;

/// How far apart two writes of a device's `last_seen` are at least
/// (1642, [`Store::device_seen`]): the mailbox page says "last seen" to
/// the minute.
pub const SEEN_EVERY_SECS: i64 = 60;

/// The most results a search returns from the store.
pub const SEARCH_LIMIT: usize = 200;

const SCHEMA: &str = "
CREATE TABLE conversations(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0,
    unread INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (identity, peer));
CREATE TABLE messages(
    seq INTEGER PRIMARY KEY,
    identity BLOB NOT NULL,
    conversation BLOB NOT NULL,
    id BLOB,
    direction INTEGER NOT NULL,
    state INTEGER,
    unproven INTEGER NOT NULL DEFAULT 0,
    verification INTEGER NOT NULL,
    last_leg INTEGER,
    timestamp REAL,
    received_at INTEGER,
    utc_offset INTEGER,
    size INTEGER NOT NULL,
    title BLOB,
    content TEXT NOT NULL,
    fields BLOB,
    error TEXT);
CREATE INDEX messages_by_conversation ON messages(identity, conversation, seq);
CREATE INDEX messages_by_id ON messages(id);
CREATE TABLE ledger(
    message INTEGER NOT NULL REFERENCES messages(seq) ON DELETE CASCADE,
    line INTEGER NOT NULL,
    at INTEGER,
    utc_offset INTEGER,
    what TEXT NOT NULL,
    PRIMARY KEY (message, line));
CREATE TABLE names(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (identity, peer));
CREATE VIRTUAL TABLE message_text USING fts5(
    content, content='messages', content_rowid='seq', tokenize='trigram');
CREATE TRIGGER message_text_insert AFTER INSERT ON messages BEGIN
    INSERT INTO message_text(rowid, content) VALUES (new.seq, new.content);
END;
CREATE TRIGGER message_text_delete AFTER DELETE ON messages BEGIN
    INSERT INTO message_text(message_text, rowid, content)
        VALUES ('delete', old.seq, old.content);
END;
CREATE TRIGGER message_text_update AFTER UPDATE OF content ON messages BEGIN
    INSERT INTO message_text(message_text, rowid, content)
        VALUES ('delete', old.seq, old.content);
    INSERT INTO message_text(rowid, content) VALUES (new.seq, new.content);
END;
";

/// What schema 2 adds to schema 1: the positions telemetry carried.
const SCHEMA_V2: &str = "
CREATE TABLE positions(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    via BLOB NOT NULL,
    message INTEGER REFERENCES messages(seq) ON DELETE SET NULL,
    fix_time INTEGER NOT NULL,
    received_at INTEGER,
    lat_e6 INTEGER NOT NULL, lon_e6 INTEGER NOT NULL,
    alt_e2 INTEGER, speed_e2 INTEGER, bearing_e2 INTEGER, accuracy_e2 INTEGER,
    telemetry BLOB NOT NULL,
    PRIMARY KEY (identity, peer, fix_time));
";

/// What schema 3 adds to schema 2: the bytes of the received messages whose
/// signature waits on their sender's key.
const SCHEMA_V3: &str = "
CREATE TABLE unverified(
    identity BLOB NOT NULL,
    id BLOB NOT NULL,
    source BLOB NOT NULL,
    packed BLOB NOT NULL,
    PRIMARY KEY (identity, id));
";

/// What schema 4 adds to schema 3: the home node's archive, its admitted
/// devices and its open admission tokens.
const SCHEMA_V4: &str = "
CREATE TABLE archive(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    identity BLOB NOT NULL,
    kind INTEGER NOT NULL,
    id BLOB NOT NULL,
    at INTEGER NOT NULL,
    packed BLOB NOT NULL);
CREATE UNIQUE INDEX archive_by_id ON archive(identity, kind, id);
CREATE TABLE home_devices(
    identity BLOB NOT NULL,
    device BLOB NOT NULL,
    name TEXT NOT NULL,
    admitted_at INTEGER NOT NULL,
    PRIMARY KEY (identity, device));
CREATE TABLE home_tokens(
    identity BLOB NOT NULL,
    token BLOB NOT NULL,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (identity, token));
";

/// What schema 5 adds to schema 4: the home node's outbox, one row a message
/// a client handed in with `/send`, keyed by the client's own id so a retried
/// submission finds its row. `state` is 0 while it is retried, 1 delivered,
/// 2 when the retries ended; `propagated` is 1 once a propagation node took
/// a copy.
const SCHEMA_V5: &str = "
CREATE TABLE home_outbox(
    identity BLOB NOT NULL,
    client_id BLOB NOT NULL,
    id BLOB NOT NULL,
    destination BLOB NOT NULL,
    packed BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    state INTEGER NOT NULL,
    propagated INTEGER NOT NULL,
    PRIMARY KEY (identity, client_id));
CREATE UNIQUE INDEX home_outbox_by_id ON home_outbox(identity, id);
";

/// What schema 6 adds to schema 5: where an imported message came from
/// (`nomadnet`, `loquax`; NULL for loquax's own rows) and what that app said
/// of its signature (1 verified, 0 not), kept apart from our own verdict in
/// `verification`.
const SCHEMA_V6: &str = "
ALTER TABLE messages ADD COLUMN origin TEXT;
ALTER TABLE messages ADD COLUMN origin_verdict INTEGER;
";

/// What schema 7 adds to schema 6: a conversation's mute (unix seconds it
/// ends, NULL for none), whether it is archived, and whether the reader
/// verified the peer's identity in person (1146).
const SCHEMA_V7: &str = "
ALTER TABLE conversations ADD COLUMN muted_until INTEGER;
ALTER TABLE conversations ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
ALTER TABLE conversations ADD COLUMN verified INTEGER NOT NULL DEFAULT 0;
";

/// What schema 8 adds to schema 7: a conversation's draft, the message
/// typed and not sent (1219): its text, the LXMF id a reply answers, and
/// what is to go with it as a JSON array (empty until attachments exist).
const SCHEMA_V8: &str = "
CREATE TABLE drafts(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    body TEXT NOT NULL,
    reply BLOB,
    attachments TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (identity, peer));
";

/// What schema 9 adds to schema 8: read receipts (1230). When the peer
/// read one of ours, its reader's clock in unix seconds, beside `state` 8
/// (read); and whether the peer's latest message carried the `lev`
/// namespace's read-receipt mark.
const SCHEMA_V9: &str = "
ALTER TABLE messages ADD COLUMN read_at INTEGER;
ALTER TABLE conversations ADD COLUMN receipts INTEGER NOT NULL DEFAULT 0;
";

/// What schema 10 adds to schema 9: a conversation's running position
/// share (1273). `span` is [`Span::word`]; `last_*` the last report it
/// sent, all null before the first.
const SCHEMA_V10: &str = "
CREATE TABLE shares(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    span TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    until INTEGER,
    last_lat INTEGER,
    last_lon INTEGER,
    last_alt INTEGER,
    last_accuracy INTEGER,
    last_fix_at INTEGER,
    last_at INTEGER,
    last_moved INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (identity, peer));
";

/// What schema 11 adds to schema 10: the groups this home hosts (1307,
/// `docs/drafts/group-chats-study-2026-11.md` 5.4), their members and the
/// fan-out's bookkeeping, one row a copy of a member's message to another
/// member. The group's private key is not here: it is a file beside the
/// identity's ([`crate::home::group`]). See [`groups`] for the columns.
const SCHEMA_V11: &str = "
CREATE TABLE groups(
    identity BLOB NOT NULL,
    address BLOB NOT NULL,
    name TEXT NOT NULL,
    admin BLOB NOT NULL,
    epoch INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (identity, address));
CREATE TABLE group_members(
    identity BLOB NOT NULL,
    grp BLOB NOT NULL,
    address BLOB NOT NULL,
    name TEXT NOT NULL,
    flags INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL,
    left_at INTEGER,
    PRIMARY KEY (identity, grp, address));
CREATE TABLE group_fanout(
    identity BLOB NOT NULL,
    grp BLOB NOT NULL,
    original BLOB NOT NULL,
    member BLOB NOT NULL,
    copy BLOB NOT NULL,
    state INTEGER NOT NULL,
    sent_at INTEGER NOT NULL,
    proven_at INTEGER,
    read_at INTEGER,
    PRIMARY KEY (identity, grp, original, member));
CREATE UNIQUE INDEX group_fanout_by_copy ON group_fanout(identity, copy);
";

/// What schema 12 adds to schema 11 (1307 G2): the digest a held copy went
/// out in (`group_fanout.digest`, null for a copy sent alone), and the
/// groups a device knows (`client_groups`), kept as the messages taught
/// them, so a restart lists every group before any of its messages is
/// loaded. `members` is the `lev` key `0x05` as the wire carries it.
const SCHEMA_V12: &str = "
ALTER TABLE group_fanout ADD COLUMN digest BLOB;
CREATE INDEX group_fanout_by_digest ON group_fanout(identity, digest);
CREATE TABLE client_groups(
    identity BLOB NOT NULL,
    address BLOB NOT NULL,
    name TEXT NOT NULL,
    admin_name TEXT NOT NULL,
    inviter BLOB,
    ours INTEGER NOT NULL,
    epoch INTEGER NOT NULL,
    standing INTEGER NOT NULL,
    members BLOB NOT NULL,
    PRIMARY KEY (identity, address));
";

/// What schema 13 adds to schema 12 (1307 pass four): whether a group's
/// copy waits at a propagation node (`group_fanout.propagated`, 1 once a
/// node took it), so a restart does not upload it again.
const SCHEMA_V13: &str = "
ALTER TABLE group_fanout ADD COLUMN propagated INTEGER NOT NULL DEFAULT 0;
";

/// What schema 14 adds to schema 13 (1385): how a position came
/// (`positions.source`, 0 in a message's telemetry, 1 announced to the
/// mesh), so the track keeps the announced fixes too and a restart gives
/// them back as such.
const SCHEMA_V14: &str = "
ALTER TABLE positions ADD COLUMN source INTEGER NOT NULL DEFAULT 0;
";

/// What schema 15 adds to schema 14 (1409): what the propagation node is
/// still to be asked about (`pn_asks`, one row per open `transient_id`:
/// our message id, the node it was uploaded to and when, unix seconds),
/// and the nodes whose announce said they answer `/lev/status`
/// (`pn_nodes`, with the version they said), so a restart asks about the
/// uploads before it ([`crate::pn_status::Asker::restore`]).
const SCHEMA_V15: &str = "
CREATE TABLE pn_asks(
    identity BLOB NOT NULL,
    transient BLOB NOT NULL,
    message BLOB NOT NULL,
    node BLOB NOT NULL,
    uploaded_at INTEGER NOT NULL,
    PRIMARY KEY (identity, transient));
CREATE TABLE pn_nodes(
    identity BLOB NOT NULL,
    node BLOB NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (identity, node));
";

/// What schema 16 adds to schema 15 (1466): when a group copy's member's
/// device said it has it (`group_fanout.device_at`, unix seconds, null
/// until then): its `0x07`, or its read receipt, which includes it. The
/// writer's two ticks count these, not the proofs, which a member's
/// mailbox signs too.
const SCHEMA_V16: &str = "
ALTER TABLE group_fanout ADD COLUMN device_at INTEGER;
";

/// What schema 17 adds to schema 16 (1490): the conversation's row as
/// the list shows it ([`crate::ui::listing::Listing`]), written beside
/// the fold that changed it, read whole by [`Store::start`] so a
/// restart reads summaries, not bodies. NULL where unknown or where a
/// write touched messages without handing a fresh row; the start read
/// falls back to a page for those and the next fold heals the column.
const SCHEMA_V17: &str = "
ALTER TABLE conversations ADD COLUMN listing BLOB;
";

/// What schema 18 adds to schema 17 (1579, [`crate::home::pages`]): the
/// NomadNet pages the home heard (`home_pages`, one row a node, at most
/// [`crate::home::pages::PAGES_CAP`]) and the identity's bookmarks with
/// their tombstones (`home_bookmarks`, keyed by lnomad's URL, `at` the
/// writing device's clock in unix milliseconds). `stamp` is the home's
/// change counter, shared by both tables: a device asks for the rows with
/// a stamp after its own.
const SCHEMA_V18: &str = "
CREATE TABLE home_pages(
    identity BLOB NOT NULL,
    dest BLOB NOT NULL,
    name TEXT,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    hops INTEGER,
    count INTEGER NOT NULL,
    stamp INTEGER NOT NULL,
    PRIMARY KEY (identity, dest));
CREATE INDEX home_pages_by_stamp ON home_pages(identity, stamp);
CREATE TABLE home_bookmarks(
    identity BLOB NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL,
    at INTEGER NOT NULL,
    deleted INTEGER NOT NULL,
    stamp INTEGER NOT NULL,
    PRIMARY KEY (identity, url));
CREATE INDEX home_bookmarks_by_stamp ON home_bookmarks(identity, stamp);
";

/// What schema 19 adds to schema 18 (1635): the petnames this device
/// set that its home node has not taken yet (`owed_petnames`, one row a
/// peer, the newest name, NULL for a cleared one), so a restart hands
/// them again and a pull of an older entry does not undo them.
const SCHEMA_V19: &str = "
CREATE TABLE owed_petnames(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    petname TEXT,
    PRIMARY KEY (identity, peer));
";

/// What schema 20 adds to schema 19 (1642): when the mailbox last heard
/// from each admitted device (`home_devices.last_seen`, unix seconds, NULL
/// until the first request after the migration), so the mailbox page of
/// every device can say which devices it knows and when each was last
/// there.
const SCHEMA_V20: &str = "
ALTER TABLE home_devices ADD COLUMN last_seen INTEGER;
";

/// What schema 21 adds to schema 20 (1640): the peer states this device
/// set that its home node has not taken yet (`owed_peer_states`, one row a
/// peer, the newest state whole, `muted_until` NULL for unmuted), as
/// `owed_petnames` holds the petnames.
const SCHEMA_V21: &str = "
CREATE TABLE owed_peer_states(
    identity BLOB NOT NULL,
    peer BLOB NOT NULL,
    blocked INTEGER NOT NULL,
    pinned INTEGER NOT NULL,
    archived INTEGER NOT NULL,
    muted_until INTEGER,
    PRIMARY KEY (identity, peer));
";

/// What schema 22 adds to schema 21 (1643, [`crate::home::pages::NameRow`]):
/// the name of every LXMF delivery announce the home heard (`home_names`,
/// one row an address, at most [`crate::home::pages::NAMES_CAP`], the one
/// heard longest ago going first). `stamp` is the counter `home_pages` and
/// `home_bookmarks` share, so the rows ride in the same `/pages` delta.
const SCHEMA_V22: &str = "
CREATE TABLE home_names(
    identity BLOB NOT NULL,
    address BLOB NOT NULL,
    name TEXT NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    hops INTEGER,
    stamp INTEGER NOT NULL,
    PRIMARY KEY (identity, address));
CREATE INDEX home_names_by_stamp ON home_names(identity, stamp);
CREATE INDEX home_names_by_last_seen ON home_names(identity, last_seen);
";

pub mod groups;
pub mod pages;

/// The model's key of the row at seq 0. The model counts keys unsigned
/// ([`Message::activity`]) and imported history sits below seq 0, so a key is
/// the seq with its sign bit flipped: the same order, and every row of an
/// older store, whose keys all lie above 0, keeps its place.
pub const KEY_OF_SEQ_ZERO: u64 = 1 << 63;

/// The model's key for a store row's `seq`.
pub fn key_of(seq: i64) -> u64 {
    (seq as u64) ^ KEY_OF_SEQ_ZERO
}

/// The store row's `seq` for a model key.
pub fn seq_of(key: u64) -> i64 {
    (key ^ KEY_OF_SEQ_ZERO) as i64
}

/// The tables a whole store of schema 1 has.
const TABLES_V1: [&str; 5] = [
    "conversations",
    "messages",
    "ledger",
    "names",
    "message_text",
];

/// The tables schema 2 adds.
const TABLES_V2: [&str; 1] = ["positions"];

/// The tables schema 3 adds.
const TABLES_V3: [&str; 1] = ["unverified"];

/// The tables schema 4 adds.
const TABLES_V4: [&str; 3] = ["archive", "home_devices", "home_tokens"];

/// The tables schema 5 adds.
const TABLES_V5: [&str; 1] = ["home_outbox"];

/// The table schema 8 adds.
const TABLES_V8: [&str; 1] = ["drafts"];

/// The table schema 10 adds.
const TABLES_V10: [&str; 1] = ["shares"];

/// The tables schema 11 adds.
const TABLES_V11: [&str; 3] = ["groups", "group_members", "group_fanout"];

/// The table schema 12 adds.
const TABLES_V12: [&str; 1] = ["client_groups"];

/// The tables schema 15 adds.
const TABLES_V15: [&str; 2] = ["pn_asks", "pn_nodes"];

/// The tables schema 18 adds.
const TABLES_V18: [&str; 2] = ["home_pages", "home_bookmarks"];

/// The table schema 19 adds.
const TABLES_V19: [&str; 1] = ["owed_petnames"];

/// The table schema 21 adds.
const TABLES_V21: [&str; 1] = ["owed_peer_states"];

/// The table schema 22 adds.
const TABLES_V22: [&str; 1] = ["home_names"];

/// What [`Store::put_hub`] writes: a hub heard, or the rooms a kept hub
/// named (1643).
#[derive(Clone, Copy)]
enum HubPut<'a> {
    Heard(&'a crate::ui::hubs::HeardHub),
    Rooms(&'a [u8; 16], &'a [String]),
}

/// A received message kept for its signature check: its id, its sender and
/// the bytes it came as.
pub type Held = ([u8; 32], [u8; 16], Vec<u8>);

/// Why the store could not be used.
#[derive(Debug)]
pub enum StoreError {
    /// The file is there and is not a whole store: loquax will not start
    /// over it.
    Corrupt { path: PathBuf, detail: String },
    /// A later loquax wrote it.
    Newer { path: PathBuf, version: i32 },
    /// Reading or writing failed.
    Failed { path: PathBuf, detail: String },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Corrupt { path, detail } => write!(
                f,
                "the message store {} is damaged ({detail}). Loquax will not start over it, \
                 because an empty history would be written where the old one was. Move the \
                 file aside to start with an empty history, or put a backup in its place",
                path.display()
            ),
            Self::Newer { path, version } => write!(
                f,
                "the message store {} was written by a later Loquax (schema {version}, this one \
                 knows {SCHEMA_VERSION}); update Loquax",
                path.display()
            ),
            Self::Failed { path, detail } => {
                write!(f, "the message store {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// The open store, scoped to one of our addresses.
pub struct Store {
    conn: Connection,
    path: PathBuf,
    identity: [u8; 16],
    /// The rows read and the commits made since the last reset (1489,
    /// [`crate::ui::counters`]); shared with the commit hook.
    ops: std::sync::Arc<Ops>,
}

impl Store {
    /// `home/history.sqlite3` for `identity`, our LXMF address.
    pub fn open_in(home: &Path, identity: [u8; 16]) -> Result<Self, StoreError> {
        Self::open(&home.join(STORE_FILE), identity)
    }

    /// The store at `path`: made when the file is missing, refused when it
    /// is there and not whole ([`StoreError::Corrupt`]).
    pub fn open(path: &Path, identity: [u8; 16]) -> Result<Self, StoreError> {
        let fresh = match std::fs::symlink_metadata(path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(failed(path, error)),
        };
        let flags = match fresh {
            true => OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            // Never create over a name that vanished between the two calls.
            false => OpenFlags::SQLITE_OPEN_READ_WRITE,
        } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(|e| failed(path, e))?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_MS))
            .map_err(|e| failed(path, e))?;
        let store = Self::over(conn, path, identity);
        match fresh {
            true => store.create()?,
            false => store.check()?,
        }
        store.configure()?;
        Ok(store)
    }

    /// The store at `path` for reading only, never made, migrated or
    /// written: what a dry run and an export look at. `None` when there is
    /// no file.
    pub fn open_read_only(path: &Path, identity: [u8; 16]) -> Result<Option<Self>, StoreError> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(failed(path, error)),
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags).map_err(|e| failed(path, e))?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_MS))
            .map_err(|e| failed(path, e))?;
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| failed(path, e))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::Newer {
                path: path.to_path_buf(),
                version,
            });
        }
        if version < SCHEMA_VERSION {
            return Err(failed(
                path,
                format!("schema {version}; start Loquax once to bring it to {SCHEMA_VERSION}"),
            ));
        }
        Ok(Some(Self::over(conn, path, identity)))
    }

    /// The store over an open connection, its commits counted where the
    /// counters are on ([`crate::ui::counters`]): every commit, a
    /// transaction's and a lone statement's alike, is one sync to the disk.
    fn over(conn: Connection, path: &Path, identity: [u8; 16]) -> Self {
        let ops = std::sync::Arc::new(Ops::new());
        if crate::ui::counters::ON {
            let counting = std::sync::Arc::clone(&ops);
            let hooked = conn.commit_hook(Some(move || {
                counting.add(Op::Commits, 1);
                // Never veto the commit.
                false
            }));
            if let Err(error) = hooked {
                tracing::debug!("STORE no commit counter: {error}");
            }
        }
        Self {
            conn,
            path: path.to_path_buf(),
            identity,
            ops,
        }
    }

    /// The rows the store's reads handed up and the commits it made since
    /// it opened or the last [`Store::reset_counters`] (1489); `None` in a
    /// release build without the `counters` feature.
    pub fn counters(&self) -> Option<Counters> {
        self.ops.read()
    }

    /// Start the store's count afresh.
    pub fn reset_counters(&self) {
        self.ops.reset();
    }

    /// `rows`, counted as read.
    fn read<T>(&self, rows: Vec<T>) -> Vec<T> {
        self.ops.add(Op::StoreRowsRead, rows.len());
        rows
    }

    /// One row read, when there was one.
    fn read_one<T>(&self, row: T) -> T {
        self.ops.add(Op::StoreRowsRead, 1);
        row
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn create(&self) -> Result<(), StoreError> {
        let batch = format!(
            "BEGIN IMMEDIATE; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} {SCHEMA_V5} \
             {SCHEMA_V6} {SCHEMA_V7} {SCHEMA_V8} {SCHEMA_V9} {SCHEMA_V10} {SCHEMA_V11} \
             {SCHEMA_V12} {SCHEMA_V13} {SCHEMA_V14} {SCHEMA_V15} {SCHEMA_V16} {SCHEMA_V17} \
             {SCHEMA_V18} {SCHEMA_V19} {SCHEMA_V20} {SCHEMA_V21} {SCHEMA_V22} \
             PRAGMA user_version = {SCHEMA_VERSION}; \
             COMMIT;"
        );
        self.conn
            .execute_batch(&batch)
            .map_err(|e| failed(&self.path, e))
    }

    /// The checks that tell a whole store from a damaged one, cheapest
    /// first, before anything reads it for the screen.
    fn check(&self) -> Result<(), StoreError> {
        let corrupt = |detail: String| StoreError::Corrupt {
            path: self.path.clone(),
            detail,
        };
        // Reading the schema makes SQLite read the header, roll back a write
        // a crash left in its journal, and refuse a file shorter than the
        // page count its header gives (`lockBtree`, "database disk image is
        // malformed"): a cut file is caught here, before anything reads it
        // for the screen.
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| corrupt(e.to_string()))?;
        let tables: Vec<String> = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .and_then(|mut statement| {
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<Vec<String>, _>>()
            })
            .map_err(|e| corrupt(e.to_string()))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::Newer {
                path: self.path.clone(),
                version,
            });
        }
        if !(1..=SCHEMA_VERSION).contains(&version) {
            return Err(corrupt(format!(
                "not a Loquax store: schema version {version}"
            )));
        }
        let missing = |want: &[&'static str]| -> Option<&'static str> {
            want.iter()
                .find(|t| !tables.iter().any(|n| n == *t))
                .copied()
        };
        if let Some(missing) = missing(&TABLES_V1) {
            return Err(corrupt(format!("table {missing} is missing")));
        }
        if version >= 2 {
            if let Some(missing) = missing(&TABLES_V2) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 3 {
            if let Some(missing) = missing(&TABLES_V3) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 4 {
            if let Some(missing) = missing(&TABLES_V4) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 5 {
            if let Some(missing) = missing(&TABLES_V5) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 8 {
            if let Some(missing) = missing(&TABLES_V8) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 10 {
            if let Some(missing) = missing(&TABLES_V10) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 11 {
            if let Some(missing) = missing(&TABLES_V11) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 12 {
            if let Some(missing) = missing(&TABLES_V12) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 15 {
            if let Some(missing) = missing(&TABLES_V15) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 18 {
            if let Some(missing) = missing(&TABLES_V18) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 19 {
            if let Some(missing) = missing(&TABLES_V19) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 21 {
            if let Some(missing) = missing(&TABLES_V21) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version >= 22 {
            if let Some(missing) = missing(&TABLES_V22) {
                return Err(corrupt(format!("table {missing} is missing")));
            }
        }
        if version < SCHEMA_VERSION {
            return self.migrate(version);
        }
        Ok(())
    }

    /// An older schema to this one: each later schema's tables, all or
    /// none. The version is read again under the write lock, so two loquax
    /// opening one old file at once migrate it once.
    ///
    /// Each migration says one line, `HOME store migrated from= to= steps=
    /// took_ms=`, and one that fails says `HOME store migration refused`
    /// before the error goes up (1305: miauhaus went from 7 to 12 with no
    /// trace but a read of `user_version`).
    fn migrate(&self, seen: i32) -> Result<(), StoreError> {
        let started = std::time::Instant::now();
        let mut from = None;
        let mut migrate = || -> rusqlite::Result<()> {
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            let version: i32 = self
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if version < SCHEMA_VERSION {
                from = Some(version);
                let mut batch = String::new();
                if version < 2 {
                    batch.push_str(SCHEMA_V2);
                }
                if version < 3 {
                    batch.push_str(SCHEMA_V3);
                }
                if version < 4 {
                    batch.push_str(SCHEMA_V4);
                }
                if version < 5 {
                    batch.push_str(SCHEMA_V5);
                }
                if version < 6 {
                    batch.push_str(SCHEMA_V6);
                }
                if version < 7 {
                    batch.push_str(SCHEMA_V7);
                }
                if version < 8 {
                    batch.push_str(SCHEMA_V8);
                }
                if version < 9 {
                    batch.push_str(SCHEMA_V9);
                }
                if version < 10 {
                    batch.push_str(SCHEMA_V10);
                }
                if version < 11 {
                    batch.push_str(SCHEMA_V11);
                }
                if version < 12 {
                    batch.push_str(SCHEMA_V12);
                }
                if version < 13 {
                    batch.push_str(SCHEMA_V13);
                }
                if version < 14 {
                    batch.push_str(SCHEMA_V14);
                }
                if version < 15 {
                    batch.push_str(SCHEMA_V15);
                }
                if version < 16 {
                    batch.push_str(SCHEMA_V16);
                }
                if version < 17 {
                    batch.push_str(SCHEMA_V17);
                }
                if version < 18 {
                    batch.push_str(SCHEMA_V18);
                }
                if version < 19 {
                    batch.push_str(SCHEMA_V19);
                }
                if version < 20 {
                    batch.push_str(SCHEMA_V20);
                }
                if version < 21 {
                    batch.push_str(SCHEMA_V21);
                }
                batch.push_str(SCHEMA_V22);
                batch.push_str(&format!("PRAGMA user_version = {SCHEMA_VERSION};"));
                if let Err(error) = self.conn.execute_batch(&batch) {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
            self.conn.execute_batch("COMMIT")
        };
        let outcome = migrate();
        // Read under the write lock: another loquax may have migrated first,
        // which says nothing here. A failure before the read is said with
        // the version the open read.
        let from = match (from, &outcome) {
            (Some(from), _) => from,
            (None, Ok(())) => return Ok(()),
            (None, Err(_)) => seen,
        };
        let took_ms = started.elapsed().as_millis();
        match outcome {
            Ok(()) => {
                tracing::info!(
                    "HOME store migrated from={from} to={SCHEMA_VERSION} steps={} took_ms={took_ms} \
                     path={}",
                    SCHEMA_VERSION - from,
                    self.path.display()
                );
                Ok(())
            }
            Err(error) => {
                tracing::warn!(
                    "HOME store migration refused from={from} to={SCHEMA_VERSION} took_ms={took_ms} \
                     path={}: {error}",
                    self.path.display()
                );
                Err(failed(&self.path, error))
            }
        }
    }

    fn configure(&self) -> Result<(), StoreError> {
        self.conn
            .execute_batch(
                "PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL; \
                 PRAGMA foreign_keys = ON;",
            )
            .map_err(|e| failed(&self.path, e))
    }

    /// The largest key the store holds, from which the model counts on.
    pub fn last_key(&self) -> Result<u64, StoreError> {
        let last: Option<i64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM messages", [], |row| row.get(0))
            .map(|row| self.read_one(row))
            .map_err(|e| self.failed(e))?;
        // Never below seq 0: imported history sits there, and the model counts
        // on from here.
        Ok(key_of(last.unwrap_or(0).max(0)))
    }

    /// Every conversation with its newest [`LOAD_WINDOW`] messages, the most
    /// recent conversation first.
    pub fn restore(&self) -> Result<Vec<Restored>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT c.peer, c.pinned, c.unread, c.muted_until, c.archived, c.verified, \
                 c.receipts, (SELECT MAX(seq) FROM messages m \
                  WHERE m.identity = c.identity AND m.conversation = c.peer) AS last \
                 FROM conversations c WHERE c.identity = ?1 \
                 ORDER BY last DESC",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, i64>(2)?,
                    Flags {
                        muted_until: row.get::<_, Option<i64>>(3)?,
                        archived: row.get::<_, bool>(4)?,
                        verified: row.get::<_, bool>(5)?,
                        receipts: row.get::<_, bool>(6)?,
                    },
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let mut groups = self.client_groups()?;
        let mut out = Vec::new();
        for (peer, pinned, unread, flags) in rows {
            let peer = self.address(&peer)?;
            let (messages, earlier) = self.page(peer, u64::MAX, None)?;
            let draft = self.draft(peer)?;
            let group = groups.remove(&peer);
            out.push(Restored {
                peer,
                pinned,
                flags,
                unread: unread.max(0) as usize,
                messages,
                earlier,
                draft,
                group,
                listing: None,
            });
        }
        // A group with no conversation row yet (only its state came) is
        // listed too, after the others.
        for (peer, group) in groups {
            out.push(Restored {
                peer,
                pinned: false,
                flags: Flags::default(),
                unread: 0,
                messages: Vec::new(),
                earlier: 0,
                draft: Draft::default(),
                group: Some(group),
                listing: None,
            });
        }
        Ok(out)
    }

    /// The conversations as a restart reads them (1490, schema 17): the
    /// summaries alone, no body. A conversation whose `listing` column
    /// is missing or does not decode (an older store, an import, a
    /// write that touched messages without a fresh row) is handed its
    /// newest page instead, as [`Store::restore`] hands every one, and
    /// the next fold writes the column. The open conversation's page
    /// follows through the same [`StoreWrite`] reads the scrollback
    /// uses.
    pub fn start(&self) -> Result<Vec<Restored>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT c.peer, c.pinned, c.unread, c.muted_until, c.archived, c.verified, \
                 c.receipts, c.listing, (SELECT MAX(seq) FROM messages m \
                  WHERE m.identity = c.identity AND m.conversation = c.peer) AS last, \
                 (SELECT COUNT(*) FROM messages m \
                  WHERE m.identity = c.identity AND m.conversation = c.peer) AS held \
                 FROM conversations c WHERE c.identity = ?1 \
                 ORDER BY last DESC",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, i64>(2)?,
                    Flags {
                        muted_until: row.get::<_, Option<i64>>(3)?,
                        archived: row.get::<_, bool>(4)?,
                        verified: row.get::<_, bool>(5)?,
                        receipts: row.get::<_, bool>(6)?,
                    },
                    row.get::<_, Option<Vec<u8>>>(7)?,
                    row.get::<_, i64>(9)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let mut groups = self.client_groups()?;
        let mut drafts = self.drafts()?;
        let mut out = Vec::new();
        for (peer, pinned, unread, flags, listing, held) in rows {
            let peer = self.address(&peer)?;
            let listing = listing.as_deref().and_then(Listing::decode);
            let (messages, earlier) = match listing.is_some() {
                true => (Vec::new(), held.max(0) as usize),
                false => self.page(peer, u64::MAX, None)?,
            };
            let draft = drafts.remove(&peer).unwrap_or_default();
            let group = groups.remove(&peer);
            out.push(Restored {
                peer,
                pinned,
                flags,
                unread: unread.max(0) as usize,
                messages,
                earlier,
                draft,
                group,
                listing,
            });
        }
        // A group with no conversation row yet (only its state came) is
        // listed too, after the others.
        for (peer, group) in groups {
            out.push(Restored {
                peer,
                pinned: false,
                flags: Flags::default(),
                unread: 0,
                messages: Vec::new(),
                earlier: 0,
                draft: Draft::default(),
                group: Some(group),
                listing: None,
            });
        }
        Ok(out)
    }

    /// The groups the device knows (1307 G2), by address, as the model
    /// last wrote them. A row whose members are no membership is damage.
    pub fn client_groups(&self) -> Result<BTreeMap<[u8; 16], GroupInfo>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT address, name, admin_name, inviter, ours, epoch, standing, members \
                 FROM client_groups WHERE identity = ?1",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let mut out = BTreeMap::new();
        for (address, name, admin_name, inviter, ours, epoch, standing, members) in rows {
            let address = self.address(&address)?;
            let corrupt = |what: &str| StoreError::Corrupt {
                path: self.path.clone(),
                detail: format!("a group's {what}"),
            };
            let (_, members) =
                GroupInfo::members_from(&members).ok_or_else(|| corrupt("members"))?;
            let standing = match standing {
                0 => Standing::Invited,
                1 => Standing::Joined,
                2 => Standing::Left,
                3 => Standing::Ended,
                _ => return Err(corrupt("standing")),
            };
            out.insert(
                address,
                GroupInfo {
                    address,
                    name,
                    admin_name,
                    inviter: inviter.map(|i| self.address(&i)).transpose()?,
                    ours,
                    epoch: u64::try_from(epoch).map_err(|_| corrupt("epoch"))?,
                    members,
                    standing,
                },
            );
        }
        Ok(out)
    }

    /// The [`LOAD_WINDOW`] messages of `peer`'s conversation before key
    /// `before`, or every one from `through` on when that reaches further,
    /// oldest first, and how many the store holds before those.
    pub fn earlier(
        &self,
        peer: [u8; 16],
        before: u64,
        through: Option<u64>,
    ) -> Result<(Vec<Message>, usize), StoreError> {
        self.page(peer, before, through)
    }

    fn page(
        &self,
        peer: [u8; 16],
        before: u64,
        through: Option<u64>,
    ) -> Result<(Vec<Message>, usize), StoreError> {
        let before = seq_of(before);
        // The window's oldest key: the LOAD_WINDOW-th before `before`, or
        // none when fewer are there, then down to `through`.
        let window_start: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM messages WHERE identity = ?1 AND conversation = ?2 \
                 AND seq < ?3 ORDER BY seq DESC LIMIT 1 OFFSET ?4",
                params![
                    &self.identity[..],
                    &peer[..],
                    before,
                    (LOAD_WINDOW - 1) as i64
                ],
                |row| row.get(0),
            )
            .optional()
            .map(|row| self.read_one(row))
            .map_err(|e| self.failed(e))?;
        let through = through.map(seq_of);
        let lowest = match (window_start, through) {
            (Some(start), Some(through)) => start.min(through),
            (Some(start), None) => start,
            (None, _) => i64::MIN,
        };
        let mut statement = self
            .conn
            .prepare(
                "SELECT seq, id, direction, state, unproven, verification, last_leg, \
                 received_at, utc_offset, size, content, fields, read_at FROM messages \
                 WHERE identity = ?1 AND conversation = ?2 AND seq < ?3 AND seq >= ?4 \
                 ORDER BY seq",
            )
            .map_err(|e| self.failed(e))?;
        let mut messages = statement
            .query_map(
                params![&self.identity[..], &peer[..], before, lowest],
                row_message,
            )
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let mut ledger = self
            .conn
            .prepare("SELECT at, utc_offset, what FROM ledger WHERE message = ?1 ORDER BY line")
            .map_err(|e| self.failed(e))?;
        for message in &mut messages {
            message.ledger = ledger
                .query_map(params![seq_of(message.activity)], |row| {
                    Ok(LedgerEntry {
                        at: wall_time(row.get(0)?, row.get(1)?),
                        what: row.get(2)?,
                    })
                })
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                .map(|rows| self.read(rows))
                .map_err(|e| self.failed(e))?;
            crate::ui::arrival::before_1462(message);
            crate::ui::groups::before_1466(message);
        }
        let first = messages.first().map_or(before, |m| seq_of(m.activity));
        let earlier: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages \
                 WHERE identity = ?1 AND conversation = ?2 AND seq < ?3",
                params![&self.identity[..], &peer[..], first],
                |row| row.get(0),
            )
            .map(|row| self.read_one(row))
            .map_err(|e| self.failed(e))?;
        Ok((messages, earlier.max(0) as usize))
    }

    /// Make the model's writes, all or none.
    pub fn write(&mut self, writes: &[StoreWrite]) -> Result<(), StoreError> {
        if writes.is_empty() {
            return Ok(());
        }
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        // A batch that touches a conversation's messages without handing
        // a fresh listing leaves the column NULL (1490): the start read
        // falls back to a page there, so an import or a foreign writer
        // can never leave a stale row on screen. Peers are resolved
        // before the deletes run.
        let mut dirty: alloc::collections::BTreeSet<[u8; 16]> = Default::default();
        let mut listed: alloc::collections::BTreeSet<[u8; 16]> = Default::default();
        for write in writes {
            match write {
                StoreWrite::Message { peer, .. } => {
                    dirty.insert(*peer);
                }
                StoreWrite::ForgetMessage { key } => {
                    let found: Option<Vec<u8>> = tx
                        .query_row(
                            "SELECT conversation FROM messages \
                             WHERE identity = ?1 AND seq = ?2",
                            params![&identity[..], seq_of(*key)],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(|e| failed(&path, e))?;
                    if let Some(peer) = found.and_then(|p| <[u8; 16]>::try_from(p.as_slice()).ok())
                    {
                        dirty.insert(peer);
                    }
                }
                StoreWrite::ForgetId { id } => {
                    let found: Option<Vec<u8>> = tx
                        .query_row(
                            "SELECT conversation FROM messages \
                             WHERE identity = ?1 AND id = ?2",
                            params![&identity[..], &id[..]],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(|e| failed(&path, e))?;
                    if let Some(peer) = found.and_then(|p| <[u8; 16]>::try_from(p.as_slice()).ok())
                    {
                        dirty.insert(peer);
                    }
                }
                StoreWrite::Listing { peer, .. } => {
                    listed.insert(*peer);
                }
                _ => {}
            }
        }
        for write in writes {
            apply(&tx, &identity, write).map_err(|e| failed(&path, e))?;
        }
        for peer in dirty.difference(&listed) {
            tx.execute(
                "UPDATE conversations SET listing = NULL \
                 WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )
            .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))
    }

    /// The peer states this device owes its home node (1640), as the last
    /// [`StoreWrite::OwedPeerStates`] kept them.
    pub fn owed_peer_states(
        &self,
    ) -> Result<BTreeMap<[u8; 16], crate::home::wire::PeerState>, StoreError> {
        let read = || -> rusqlite::Result<BTreeMap<[u8; 16], crate::home::wire::PeerState>> {
            let mut statement = self.conn.prepare(
                "SELECT peer, blocked, pinned, archived, muted_until FROM owed_peer_states \
                 WHERE identity = ?1",
            )?;
            let rows = statement.query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    crate::home::wire::PeerState {
                        blocked: row.get(1)?,
                        pinned: row.get(2)?,
                        archived: row.get(3)?,
                        muted_until: row.get(4)?,
                    },
                ))
            })?;
            let mut owed = BTreeMap::new();
            for row in rows {
                let (peer, state) = row?;
                // A row of another length is no address: passed over.
                if let Ok(peer) = <[u8; 16]>::try_from(peer.as_slice()) {
                    owed.insert(peer, state);
                }
            }
            Ok(owed)
        };
        read().map_err(|e| self.failed(e))
    }

    /// The petnames this device owes its home node (1635), as the last
    /// [`StoreWrite::OwedPetnames`] kept them.
    pub fn owed_petnames(&self) -> Result<BTreeMap<[u8; 16], Option<String>>, StoreError> {
        let read = || -> rusqlite::Result<BTreeMap<[u8; 16], Option<String>>> {
            let mut statement = self
                .conn
                .prepare("SELECT peer, petname FROM owed_petnames WHERE identity = ?1")?;
            let rows = statement.query_map(params![&self.identity[..]], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            let mut owed = BTreeMap::new();
            for row in rows {
                let (peer, petname) = row?;
                // A row of another length is no address: passed over.
                if let Ok(peer) = <[u8; 16]>::try_from(peer.as_slice()) {
                    owed.insert(peer, petname);
                }
            }
            Ok(owed)
        };
        read().map_err(|e| self.failed(e))
    }

    /// What the propagation node is still to be asked about (1409), as
    /// [`Store::save_pn_asks`] kept it.
    pub fn pn_asks(&self) -> Result<crate::pn_status::AskerState, StoreError> {
        read_pn_asks(&self.conn, &self.identity).map_err(|e| self.failed(e))
    }

    /// Keep what the propagation node is still to be asked about, all of
    /// it in place of what was kept.
    pub fn save_pn_asks(&mut self, state: &crate::pn_status::AskerState) -> Result<(), StoreError> {
        self.write(&[StoreWrite::PnAsks(state.clone())])
    }

    /// Whether a message received with this id is already held: the
    /// mailbox copy of one that came directly, in this run or an earlier one
    /// (leviculum 1070). The screen holds only a window of each conversation,
    /// so its own check cannot say this for the older ones.
    pub fn holds_received(&self, id: &[u8; 32]) -> Result<bool, StoreError> {
        received_key(&self.conn, &self.identity, id, None)
            .map(|key| key.is_some())
            .map_err(|e| self.failed(e))
    }

    /// Keep the bytes of a received message whose sender's key is not known,
    /// for [`Store::unverified`] to hand back. Kept once: a second copy of
    /// the same message changes nothing.
    pub fn keep_unverified(
        &mut self,
        id: &[u8; 32],
        source: &[u8; 16],
        packed: &[u8],
    ) -> Result<(), StoreError> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO unverified(identity, id, source, packed) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![&self.identity[..], &id[..], &source[..], packed],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// Every kept message still waiting on its sender's key. One whose
    /// message was deleted is not handed back.
    pub fn unverified(&self) -> Result<Vec<Held>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT u.id, u.source, u.packed FROM unverified u WHERE u.identity = ?1 \
                 AND EXISTS (SELECT 1 FROM messages m WHERE m.identity = u.identity \
                 AND m.id = u.id AND m.direction = 0 AND m.verification = 0)",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .filter_map(|(id, source, packed)| {
                Some((
                    <[u8; 32]>::try_from(id.as_slice()).ok()?,
                    <[u8; 16]>::try_from(source.as_slice()).ok()?,
                    packed,
                ))
            })
            .map(Ok)
            .collect()
    }

    /// A kept message's verdict: its row says it, whether or not the screen
    /// holds the message, and its bytes go.
    pub fn settle_verdict(&mut self, id: &[u8; 32], valid: bool) -> Result<(), StoreError> {
        let signature = match valid {
            true => Signature::Verified,
            false => Signature::Invalid,
        };
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        tx.execute(
            "UPDATE messages SET verification = ?1 \
             WHERE identity = ?2 AND id = ?3 AND direction = 0 AND verification = 0",
            params![signature_code(signature), &identity[..], &id[..]],
        )
        .map_err(|e| failed(&path, e))?;
        tx.execute(
            "DELETE FROM unverified WHERE identity = ?1 AND id = ?2",
            params![&identity[..], &id[..]],
        )
        .map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))
    }

    /// Every peer's latest position, by [`Fix::newer_than`], the clamp for
    /// a fix ahead of our clock applied as at receive time.
    pub fn positions(&self) -> Result<Vec<Fix>, StoreError> {
        let mut latest: BTreeMap<[u8; 16], Fix> = BTreeMap::new();
        for fix in self.fixes(None)? {
            let newer = latest
                .get(&fix.peer)
                .is_none_or(|held| fix.newer_than(held));
            if newer {
                latest.insert(fix.peer, fix);
            }
        }
        Ok(latest.into_values().collect())
    }

    /// `peer`'s newest `limit` positions by fix time, newest first: the
    /// contact screen's list (1146).
    pub fn track(&self, peer: [u8; 16], limit: usize) -> Result<Vec<Fix>, StoreError> {
        let mut fixes = self.fixes(Some(peer))?;
        fixes.sort_by_key(|fix| core::cmp::Reverse(fix.fix_time()));
        fixes.truncate(limit);
        Ok(fixes)
    }

    /// Every position the store holds, or `peer`'s only.
    fn fixes(&self, peer: Option<[u8; 16]>) -> Result<Vec<Fix>, StoreError> {
        self.fixes_between(peer, i64::MIN, i64::MAX)
    }

    /// The positions whose fix time is in `[from, to)`, `peer`'s or every
    /// peer's, by fix time: the track's reader (1385). The table's key
    /// starts with the peer and the fix time, so a window is a range.
    pub fn fixes_between(
        &self,
        peer: Option<[u8; 16]>,
        from: i64,
        to: i64,
    ) -> Result<Vec<Fix>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT peer, via, message, fix_time, received_at, lat_e6, lon_e6, alt_e2, \
                 speed_e2, bearing_e2, accuracy_e2, telemetry, source FROM positions \
                 WHERE identity = ?1 AND (?2 IS NULL OR peer = ?2) \
                 AND fix_time >= ?3 AND fix_time < ?4 ORDER BY fix_time",
            )
            .map_err(|e| self.failed(e))?;
        let peer = peer.map(|peer| peer.to_vec());
        let rows = statement
            .query_map(params![&self.identity[..], peer, from, to], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    Location {
                        last_update: row.get(3)?,
                        latitude_e6: row.get(5)?,
                        longitude_e6: row.get(6)?,
                        altitude_e2: row.get::<_, Option<i32>>(7)?.unwrap_or(0),
                        speed_e2: row.get::<_, Option<u32>>(8)?.unwrap_or(0),
                        bearing_e2: row.get::<_, Option<i32>>(9)?.unwrap_or(0),
                        accuracy_e2: row.get::<_, Option<u16>>(10)?.unwrap_or(0),
                    },
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Vec<u8>>(11)?,
                    row.get::<_, i64>(12)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .map(
                |(peer, via, message, location, received_at, telemetry, source)| {
                    Ok(Fix {
                        peer: self.address(&peer)?,
                        via: self.address(&via)?,
                        message: message.map(key_of),
                        received_at,
                        location,
                        telemetry,
                        source: source_of(source),
                    })
                },
            )
            .collect()
    }

    /// The positions we shared (1201), newest first, at most `limit`: who
    /// got one, when we sent it (unix seconds, when the store knows), and
    /// where we were. The privacy page's list; read from our own messages
    /// that carry telemetry, so a deleted message leaves it.
    pub fn shared(&self, limit: usize) -> Result<Vec<(Fix, Option<i64>)>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT seq, conversation, received_at, fields FROM messages \
                 WHERE identity = ?1 AND direction = 1 AND fields IS NOT NULL \
                 ORDER BY seq DESC",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let mut shared = Vec::new();
        for (seq, peer, sent, packed) in rows {
            if shared.len() >= limit {
                break;
            }
            let Some(fields) = unpack_fields(&packed) else {
                continue;
            };
            let read = crate::telemetry::read(self.identity, &fields, sent);
            if let Some(fix) = crate::telemetry::shown_fix(&read) {
                let mut fix = fix.clone();
                // Who got it: the conversation, not who measured.
                fix.via = self.address(&peer)?;
                fix.message = Some(key_of(seq));
                shared.push((fix, sent));
            }
        }
        Ok(shared)
    }

    /// How many positions the store holds for `peer`: the track.
    pub fn track_len(&self, peer: [u8; 16]) -> Result<usize, StoreError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM positions WHERE identity = ?1 AND peer = ?2",
                params![&self.identity[..], &peer[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(count.max(0) as usize)
    }

    /// The names heard from announces in earlier runs.
    pub fn names(&self) -> Result<Vec<([u8; 16], String)>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT peer, name FROM names WHERE identity = ?1")
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .map(|(peer, name)| Ok((self.address(&peer)?, name)))
            .collect()
    }

    /// Keep the name `peer` announced, sanitised.
    pub fn save_name(&mut self, peer: [u8; 16], name: &str) -> Result<(), StoreError> {
        self.conn
            .execute(
                "INSERT INTO names(identity, peer, name) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity, peer) DO UPDATE SET name = excluded.name \
                 WHERE name != excluded.name",
                params![&self.identity[..], &peer[..], name],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// The messages whose text has `query` in it, ignoring case, that lie
    /// below what the screen loaded: before `loaded_from`'s key in a
    /// conversation it names, and none in a conversation it does not
    /// (the screen holds all of that one). The newest first, at most
    /// [`SEARCH_LIMIT`].
    pub fn search(
        &self,
        query: &str,
        loaded_from: &BTreeMap<[u8; 16], u64>,
    ) -> Result<Vec<StoredHit>, StoreError> {
        if query.is_empty() || loaded_from.is_empty() {
            return Ok(Vec::new());
        }
        // The trigram index matches a substring of three characters or
        // more; a shorter one is a scan, as the screen's matcher is.
        let (sql, pattern) = match query.chars().count() >= 3 {
            true => (
                "SELECT m.seq, m.conversation, m.direction, m.received_at, m.utc_offset, \
                 m.content FROM message_text JOIN messages m ON m.seq = message_text.rowid \
                 WHERE message_text MATCH ?1 AND m.identity = ?2 ORDER BY m.seq DESC",
                format!("\"{}\"", query.replace('"', "\"\"")),
            ),
            false => (
                "SELECT seq, conversation, direction, received_at, utc_offset, content \
                 FROM messages WHERE content LIKE ?1 ESCAPE '\\' AND identity = ?2 \
                 ORDER BY seq DESC",
                format!(
                    "%{}%",
                    query
                        .replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                ),
            ),
        };
        let mut statement = self.conn.prepare(sql).map_err(|e| self.failed(e))?;
        let mut rows = statement
            .query(params![pattern, &self.identity[..]])
            .map_err(|e| self.failed(e))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| self.failed(e))? {
            self.ops.add(Op::StoreRowsRead, 1);
            let key: i64 = row.get(0).map_err(|e| self.failed(e))?;
            let peer: Vec<u8> = row.get(1).map_err(|e| self.failed(e))?;
            let peer = self.address(&peer)?;
            let below = loaded_from
                .get(&peer)
                .is_some_and(|&first| key_of(key) < first);
            if !below {
                continue;
            }
            let direction: i64 = row.get(2).map_err(|e| self.failed(e))?;
            let at = wall_time(
                row.get(3).map_err(|e| self.failed(e))?,
                row.get(4).map_err(|e| self.failed(e))?,
            );
            out.push(StoredHit {
                peer,
                key: key_of(key),
                outbound: direction == 1,
                body: row.get(5).map_err(|e| self.failed(e))?,
                at,
            });
            if out.len() == SEARCH_LIMIT {
                break;
            }
        }
        Ok(out)
    }

    /// The running position shares (1273), by conversation. A row this
    /// loquax cannot read (a span it does not know, half a last report) is
    /// refused as corrupt, as every other row is.
    pub fn shares(&self) -> Result<Vec<([u8; 16], Share)>, StoreError> {
        type Row = (Vec<u8>, String, i64, Option<i64>, [Option<i64>; 6], bool);
        let mut statement = self
            .conn
            .prepare(
                "SELECT peer, span, started_at, until, last_lat, last_lon, last_alt, \
                 last_accuracy, last_fix_at, last_at, last_moved FROM shares \
                 WHERE identity = ?1 ORDER BY peer",
            )
            .map_err(|e| self.failed(e))?;
        let rows: Vec<Row> = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    [
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ],
                    row.get(10)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let corrupt = |detail: String| StoreError::Corrupt {
            path: self.path.clone(),
            detail,
        };
        let mut out = Vec::new();
        for (peer, span, started_at, until, last, moved) in rows {
            let peer = self.address(&peer)?;
            let span =
                Span::of_word(&span).ok_or_else(|| corrupt(format!("a share's span {span:?}")))?;
            let last = match last {
                [None, None, None, None, None, None] => None,
                [Some(lat), Some(lon), Some(alt), Some(accuracy), Some(fix_at), Some(at)] => {
                    Some(Report {
                        fix: Location::saturating(lat, lon, alt, 0, 0, accuracy, fix_at),
                        at,
                        moved,
                    })
                }
                _ => return Err(corrupt("a share's last report is half there".to_string())),
            };
            out.push((
                peer,
                Share {
                    span,
                    started_at,
                    until,
                    last,
                },
            ));
        }
        Ok(out)
    }

    /// `peer`'s draft (1219), the empty one for none.
    /// Every conversation's draft in one read: what the start read
    /// joins, a row per draft that exists instead of an ask per
    /// conversation (1490).
    fn drafts(&self) -> Result<BTreeMap<[u8; 16], Draft>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT peer, body, reply, attachments FROM drafts WHERE identity = ?1")
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let mut out = BTreeMap::new();
        for (peer, text, reply, attachments) in rows {
            let peer = self.address(&peer)?;
            let reply = match reply {
                Some(id) => Some(id.as_slice().try_into().map_err(|_| StoreError::Corrupt {
                    path: self.path.clone(),
                    detail: format!("a draft's reply id of {} bytes", id.len()),
                })?),
                None => None,
            };
            let attachments =
                serde_json::from_str(&attachments).map_err(|e| StoreError::Corrupt {
                    path: self.path.clone(),
                    detail: format!("a draft's attachments: {e}"),
                })?;
            out.insert(
                peer,
                Draft {
                    text,
                    reply,
                    attachments,
                },
            );
        }
        Ok(out)
    }

    fn draft(&self, peer: [u8; 16]) -> Result<Draft, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT body, reply, attachments FROM drafts WHERE identity = ?1 AND peer = ?2",
                params![&self.identity[..], &peer[..]],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map(|row| self.read_one(row))
            .map_err(|e| self.failed(e))?;
        let Some((text, reply, attachments)) = row else {
            return Ok(Draft::default());
        };
        let reply = match reply {
            Some(id) => Some(id.as_slice().try_into().map_err(|_| StoreError::Corrupt {
                path: self.path.clone(),
                detail: format!("a draft's reply id of {} bytes", id.len()),
            })?),
            None => None,
        };
        let attachments = serde_json::from_str(&attachments).map_err(|e| StoreError::Corrupt {
            path: self.path.clone(),
            detail: format!("a draft's attachments: {e}"),
        })?;
        Ok(Draft {
            text,
            reply,
            attachments,
        })
    }

    fn address(&self, bytes: &[u8]) -> Result<[u8; 16], StoreError> {
        bytes.try_into().map_err(|_| StoreError::Corrupt {
            path: self.path.clone(),
            detail: format!("an address of {} bytes", bytes.len()),
        })
    }

    fn failed(&self, error: impl fmt::Display) -> StoreError {
        failed(&self.path, error)
    }
}

/// A device the home node admitted: its RNS identity hash, the name it was
/// admitted under, and when (unix seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub device: [u8; 16],
    pub name: String,
    pub admitted_at: i64,
    /// When the mailbox last heard from it (1642), unix seconds; `None`
    /// before its first request since schema 20.
    pub last_seen: Option<i64>,
}

/// What redeeming an admission token came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redeemed {
    /// The token was open; the device is admitted under this name and the
    /// token is gone.
    Admitted { name: String },
    /// No such token: never issued, or used already.
    Unknown,
    /// The token was issued for `name` but is older than its lifetime; it
    /// is gone now.
    Expired { name: String },
}

/// One open admission token, as `loquax home tokens` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenToken {
    pub token: [u8; 16],
    pub name: String,
    pub created_at: i64,
}

/// One message in the home node's outbox that is still retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub id: [u8; 32],
    pub destination: [u8; 16],
    pub packed: Vec<u8>,
    pub created_at: i64,
    pub propagated: bool,
}

/// What handing a message in with `/send` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// A new outbox row, under this archive sequence.
    New { seq: Option<u64> },
    /// The client's id was handed in before: this is the message it made.
    Again { id: [u8; 32] },
}

/// What `loquax home revoke <device>` matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revoked {
    /// Nothing by that name or hash.
    None,
    /// This one device, now removed.
    One(Device),
    /// More than one device matched; none was removed.
    Ambiguous(Vec<Device>),
}

/// What `loquax home revoke-token <prefix>` matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenRevoked {
    /// Fewer than four hex characters, or a character that is none.
    TooShort,
    /// No token starts so.
    None,
    /// This one token, now withdrawn.
    One(OpenToken),
    /// More than one token starts so; none was withdrawn.
    Ambiguous(Vec<OpenToken>),
}

/// The home node's half of the store
/// (`docs/src/concepts/loquax-home-node.md`): the archive its clients pull,
/// the admitted devices, the open admission tokens. Every row is scoped by
/// our own address, as the rest of the store is.
impl Store {
    /// Append one archive entry and evict the oldest entries while the
    /// archive holds more than `cap_bytes` of packed messages. The entry
    /// just appended is never evicted by its own append, so one message
    /// larger than the cap is still kept until the next one comes.
    ///
    /// The same `(kind, id)` twice is one entry: a message that came direct
    /// and again out of the propagation node is archived once. `None` is
    /// that second time.
    pub fn archive_append(
        &mut self,
        kind: u8,
        id: &[u8; 32],
        packed: &[u8],
        at: i64,
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let seq = append_in(&tx, &identity, kind, id, packed, at, cap_bytes)
            .map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(seq)
    }

    /// Append a name entry ([`crate::home::KIND_NAME`]) for the display
    /// name `peer` announced, heard at `at`, when it says something new: a
    /// name that sanitises to nothing, or to the name kept for `peer`
    /// already, appends nothing (`None`). The entry carries the name as
    /// announced, which each client sanitises as it does an announce; the
    /// names table keeps the sanitised one, as on any device.
    pub fn archive_name(
        &mut self,
        peer: [u8; 16],
        name: &str,
        at: i64,
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        let Some(shown) = crate::names::sanitise(name) else {
            return Ok(None);
        };
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let kept: Option<String> = tx
            .query_row(
                "SELECT name FROM names WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| failed(&path, e))?;
        if kept.as_deref() == Some(shown.as_str()) {
            return Ok(None);
        }
        let packed = crate::home::wire::name_entry(&peer, name);
        // A name may come back after another: the time makes it a new entry.
        let mut keyed = packed.clone();
        keyed.extend_from_slice(&at.to_be_bytes());
        let id = leviculum_core::crypto::sha256(&keyed);
        let seq = append_in(
            &tx,
            &identity,
            crate::home::KIND_NAME,
            &id,
            &packed,
            at,
            cap_bytes,
        )
        .map_err(|e| failed(&path, e))?;
        if let Some(seq) = seq {
            tx.execute(
                "INSERT INTO names(identity, peer, name) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity, peer) DO UPDATE SET name = excluded.name",
                params![&identity[..], &peer[..], shown],
            )
            .map_err(|e| failed(&path, e))?;
            // Only the newest name of a peer is kept: a client behind the
            // cursor needs the name it shows, not each one before it.
            let prefix = crate::home::wire::name_entry_prefix(&peer);
            tx.execute(
                "DELETE FROM archive WHERE identity = ?1 AND kind = ?2 AND seq <> ?3 \
                 AND substr(packed, 1, ?4) = ?5",
                params![
                    &identity[..],
                    i64::from(crate::home::KIND_NAME),
                    i64::try_from(seq).unwrap_or(i64::MAX),
                    i64::try_from(prefix.len()).unwrap_or(i64::MAX),
                    &prefix[..]
                ],
            )
            .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(seq)
    }

    /// Append a petname entry ([`crate::home::KIND_PETNAME`]) a client
    /// handed in for `peer`, at `at`, when it says something new: the same
    /// bytes as the peer's newest petname entry append nothing (`None`).
    /// Only the newest of a peer is kept, as for names: a client behind the
    /// cursor needs the name the reader gave last.
    pub fn archive_petname(
        &mut self,
        peer: [u8; 16],
        packed: &[u8],
        at: i64,
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        self.archive_newest_of(crate::home::KIND_PETNAME, peer, packed, at, cap_bytes)
    }

    /// Append a hub entry ([`crate::home::KIND_HUB`], 1599) for a hub the
    /// home heard announcing, when it says something new: a hub not kept
    /// yet, or with another name or hops, or heard
    /// [`crate::home::HUB_REFRESH_SECS`] after its kept hearing. Its first
    /// hearing stays the kept one, and so do the rooms its kept entry
    /// carries (1643). Only a hub's newest entry is kept, and past
    /// [`crate::ui::hubs::HEARD_BOUND`] hubs the one heard longest ago goes.
    /// `None` when nothing was appended.
    pub fn archive_hub(
        &mut self,
        hub: &crate::ui::hubs::HeardHub,
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        self.put_hub(HubPut::Heard(hub), cap_bytes)
    }

    /// The rooms a kept hub's answer to `/list` named when the home asked
    /// it (1643): its entry again with them, when they differ from the
    /// kept entry's. `None` when nothing was appended: the rooms are the
    /// kept ones, or the archive keeps no entry of the hub (it went past
    /// the bound while the question was out).
    pub fn archive_hub_rooms(
        &mut self,
        hub: &[u8; 16],
        rooms: &[String],
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        self.put_hub(HubPut::Rooms(hub, rooms), cap_bytes)
    }

    fn put_hub(&mut self, put: HubPut<'_>, cap_bytes: u64) -> Result<Option<u64>, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let kind = i64::from(crate::home::KIND_HUB);
        let hash = match put {
            HubPut::Heard(hub) => hub.hash,
            HubPut::Rooms(hash, _) => *hash,
        };
        let prefix = crate::home::wire::name_entry_prefix(&hash);
        let prefix_len = i64::try_from(prefix.len()).unwrap_or(i64::MAX);
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let newest: Option<Vec<u8>> = tx
            .query_row(
                "SELECT packed FROM archive WHERE identity = ?1 AND kind = ?2 \
                 AND substr(packed, 1, ?3) = ?4 ORDER BY seq DESC LIMIT 1",
                params![&identity[..], kind, prefix_len, &prefix[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| failed(&path, e))?;
        let kept = newest
            .as_deref()
            .and_then(|packed| crate::home::wire::read_hub_entry_rooms(packed).ok());
        let (entry, rooms) = match (put, kept) {
            (HubPut::Heard(hub), None) => (hub.clone(), None),
            (HubPut::Heard(hub), Some((kept, rooms))) => {
                let same = kept.name == hub.name.clone().or(kept.name.clone())
                    && kept.hops == hub.hops.or(kept.hops);
                if same && hub.last < kept.last.saturating_add(crate::home::HUB_REFRESH_SECS) {
                    return Ok(None);
                }
                let mut entry = hub.clone();
                entry.first = kept.first.min(hub.first);
                entry.name = hub.name.clone().or(kept.name.clone());
                entry.hops = hub.hops.or(kept.hops);
                (entry, rooms)
            }
            (HubPut::Rooms(..), None) => return Ok(None),
            (HubPut::Rooms(_, rooms), Some((kept, kept_rooms))) => {
                if kept_rooms.as_deref() == Some(rooms) {
                    return Ok(None);
                }
                (kept, Some(rooms.to_vec()))
            }
        };
        let packed = crate::home::wire::hub_entry_rooms(&entry, rooms.as_deref());
        let mut keyed = packed.clone();
        keyed.extend_from_slice(&entry.last.to_be_bytes());
        let id = leviculum_core::crypto::sha256(&keyed);
        let seq = append_in(
            &tx,
            &identity,
            crate::home::KIND_HUB,
            &id,
            &packed,
            entry.last,
            cap_bytes,
        )
        .map_err(|e| failed(&path, e))?;
        if let Some(seq) = seq {
            tx.execute(
                "DELETE FROM archive WHERE identity = ?1 AND kind = ?2 AND seq <> ?3 \
                 AND substr(packed, 1, ?4) = ?5",
                params![
                    &identity[..],
                    kind,
                    i64::try_from(seq).unwrap_or(i64::MAX),
                    prefix_len,
                    &prefix[..]
                ],
            )
            .map_err(|e| failed(&path, e))?;
            // The bound: the hubs heard longest ago go.
            let bound = i64::try_from(crate::ui::hubs::HEARD_BOUND).unwrap_or(i64::MAX);
            tx.execute(
                "DELETE FROM archive WHERE identity = ?1 AND kind = ?2 AND seq IN \
                 (SELECT seq FROM archive WHERE identity = ?1 AND kind = ?2 \
                  ORDER BY at DESC, seq DESC LIMIT -1 OFFSET ?3)",
                params![&identity[..], kind, bound],
            )
            .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(seq)
    }

    /// The hubs the archive keeps ([`crate::home::KIND_HUB`]), last heard
    /// first.
    pub fn archived_hubs(&self) -> Result<Vec<crate::ui::hubs::HeardHub>, StoreError> {
        let path = self.path.clone();
        let mut statement = self
            .conn
            .prepare(
                "SELECT packed FROM archive WHERE identity = ?1 AND kind = ?2 \
                 ORDER BY at DESC, seq DESC",
            )
            .map_err(|e| failed(&path, e))?;
        let rows = statement
            .query_map(
                params![&self.identity[..], i64::from(crate::home::KIND_HUB)],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .map_err(|e| failed(&path, e))?;
        let mut hubs = Vec::new();
        for packed in rows {
            let packed = packed.map_err(|e| failed(&path, e))?;
            if let Ok(hub) = crate::home::wire::read_hub_entry(&packed) {
                hubs.push(hub);
            }
        }
        Ok(hubs)
    }

    /// Whether the archive holds a petname entry for `peer`, a cleared one
    /// too: the reader said their word on that peer's name (1159).
    pub fn archive_holds_petname(&self, peer: &[u8; 16]) -> Result<bool, StoreError> {
        self.archive_holds_of(crate::home::KIND_PETNAME, peer)
    }

    /// Whether the archive holds a peer state entry for `peer`: a device
    /// said its word on that peer's block, pin, archive and mute (1162).
    pub fn archive_holds_peer_state(&self, peer: &[u8; 16]) -> Result<bool, StoreError> {
        self.archive_holds_of(crate::home::KIND_PEERSTATE, peer)
    }

    /// The bytes of the archive entry of `kind` kept under `id`, while the
    /// archive still holds it.
    pub fn archive_entry(&self, kind: u8, id: &[u8; 32]) -> Result<Option<Vec<u8>>, StoreError> {
        self.conn
            .query_row(
                "SELECT packed FROM archive WHERE identity = ?1 AND kind = ?2 AND id = ?3",
                params![&self.identity[..], i64::from(kind), &id[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| self.failed(e))
    }

    /// Whether a seen mark in the archive names the message `id` (1234):
    /// some device saw it already, and its read receipt went then.
    pub fn archive_seen(&self, id: &[u8; 32]) -> Result<bool, StoreError> {
        self.archive_marked(&crate::home::wire::SEEN_MARK_PREFIX, id)
    }

    /// Whether an arrived mark in the archive names `id` (1462): a device
    /// said it has it, so its sender's device receipt went.
    pub fn archive_arrived(&self, id: &[u8; 32]) -> Result<bool, StoreError> {
        self.archive_marked(&crate::home::wire::ARRIVED_MARK_PREFIX, id)
    }

    /// Whether a mark beginning with `prefix` in the archive names `id`.
    fn archive_marked(&self, prefix: &[u8; 2], id: &[u8; 32]) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM archive WHERE identity = ?1 AND kind = ?2 \
                 AND substr(packed, 1, ?3) = ?4 AND instr(packed, ?5) > 0 LIMIT 1",
                params![
                    &self.identity[..],
                    i64::from(crate::home::KIND_MARK),
                    prefix.len() as i64,
                    &prefix[..],
                    &id[..]
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        Ok(found.is_some())
    }

    /// Whether the archive holds an entry of `kind` whose bytes begin with
    /// `peer`'s [`crate::home::wire::name_entry_prefix`].
    fn archive_holds_of(&self, kind: u8, peer: &[u8; 16]) -> Result<bool, StoreError> {
        let prefix = crate::home::wire::name_entry_prefix(peer);
        let prefix_len = i64::try_from(prefix.len()).unwrap_or(i64::MAX);
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM archive WHERE identity = ?1 AND kind = ?2 \
                 AND substr(packed, 1, ?3) = ?4 LIMIT 1",
                params![&self.identity[..], i64::from(kind), prefix_len, &prefix[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        Ok(found.is_some())
    }

    /// Append a peer state entry ([`crate::home::KIND_PEERSTATE`]) a client
    /// handed in for `peer`, as [`Store::archive_petname`] appends a
    /// petname: only the newest of a peer is kept (1146).
    pub fn archive_peer_state(
        &mut self,
        peer: [u8; 16],
        packed: &[u8],
        at: i64,
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        self.archive_newest_of(crate::home::KIND_PEERSTATE, peer, packed, at, cap_bytes)
    }

    /// Append an entry of `kind` for `peer` whose bytes begin with
    /// [`crate::home::wire::name_entry_prefix`], keeping only the newest of
    /// that kind and peer; the same bytes as the newest append nothing.
    fn archive_newest_of(
        &mut self,
        archive_kind: u8,
        peer: [u8; 16],
        packed: &[u8],
        at: i64,
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let kind = i64::from(archive_kind);
        let prefix = crate::home::wire::name_entry_prefix(&peer);
        let prefix_len = i64::try_from(prefix.len()).unwrap_or(i64::MAX);
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let newest: Option<Vec<u8>> = tx
            .query_row(
                "SELECT packed FROM archive WHERE identity = ?1 AND kind = ?2 \
                 AND substr(packed, 1, ?3) = ?4 ORDER BY seq DESC LIMIT 1",
                params![&identity[..], kind, prefix_len, &prefix[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| failed(&path, e))?;
        if newest.as_deref() == Some(packed) {
            return Ok(None);
        }
        // A name may come back after another: the time makes it a new entry.
        let mut keyed = packed.to_vec();
        keyed.extend_from_slice(&at.to_be_bytes());
        let id = leviculum_core::crypto::sha256(&keyed);
        let seq = append_in(&tx, &identity, archive_kind, &id, packed, at, cap_bytes)
            .map_err(|e| failed(&path, e))?;
        if let Some(seq) = seq {
            tx.execute(
                "DELETE FROM archive WHERE identity = ?1 AND kind = ?2 AND seq <> ?3 \
                 AND substr(packed, 1, ?4) = ?5",
                params![
                    &identity[..],
                    kind,
                    i64::try_from(seq).unwrap_or(i64::MAX),
                    prefix_len,
                    &prefix[..]
                ],
            )
            .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(seq)
    }

    /// The archive entries after `cursor`, oldest first, up to about
    /// `max_bytes` of packed messages but always at least one when there is
    /// one, and whether more follow. `first` is the oldest sequence still
    /// held, so a client whose cursor fell behind an eviction can say so.
    pub fn archive_since(
        &self,
        cursor: u64,
        max_bytes: usize,
    ) -> Result<crate::home::wire::Page, StoreError> {
        self.archive_since_folded(cursor, max_bytes, crate::home::wire::Fold::All)
    }

    /// [`Self::archive_since`] for a device that folds (1439): only the
    /// entries `fold` keeps, skipped in the query, so a board past an
    /// archive's imported head gets its first page of names at once.
    pub fn archive_since_folded(
        &self,
        cursor: u64,
        max_bytes: usize,
        fold: crate::home::wire::Fold,
    ) -> Result<crate::home::wire::Page, StoreError> {
        let cursor = i64::try_from(cursor).unwrap_or(i64::MAX);
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT seq, kind, at, packed FROM archive WHERE identity = ?1 AND seq > ?2{} \
                 ORDER BY seq",
                fold_clause(fold)
            ))
            .map_err(|e| self.failed(e))?;
        let mut rows = statement
            .query(params![&self.identity[..], cursor])
            .map_err(|e| self.failed(e))?;
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        while let Some(row) = rows.next().map_err(|e| self.failed(e))? {
            self.ops.add(Op::StoreRowsRead, 1);
            let packed: Vec<u8> = row.get(3).map_err(|e| self.failed(e))?;
            if !entries.is_empty() && bytes.saturating_add(packed.len()) > max_bytes {
                more = true;
                break;
            }
            bytes = bytes.saturating_add(packed.len());
            let seq: i64 = row.get(0).map_err(|e| self.failed(e))?;
            let kind: i64 = row.get(1).map_err(|e| self.failed(e))?;
            entries.push(crate::home::wire::Entry {
                seq: seq as u64,
                kind: u8::try_from(kind).unwrap_or(u8::MAX),
                at: row.get(2).map_err(|e| self.failed(e))?,
                packed,
            });
        }
        let first: Option<i64> = self
            .conn
            .query_row(
                "SELECT MIN(seq) FROM archive WHERE identity = ?1",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(crate::home::wire::Page {
            entries,
            more,
            first: first.map_or(0, |seq| seq as u64),
            left: None,
        })
    }

    /// How many archive entries come after `seq` (1415): what a paged
    /// `/sync` answer says still waits.
    pub fn archive_count_after(&self, seq: u64) -> Result<u64, StoreError> {
        self.archive_count_after_folded(seq, crate::home::wire::Fold::All)
    }

    /// How many entries a device that folds as `fold` still gets after
    /// `seq` (1439): `left` as that device will see it arrive.
    pub fn archive_count_after_folded(
        &self,
        seq: u64,
        fold: crate::home::wire::Fold,
    ) -> Result<u64, StoreError> {
        let seq = i64::try_from(seq).unwrap_or(i64::MAX);
        let count: i64 = self
            .conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM archive WHERE identity = ?1 AND seq > ?2{}",
                    fold_clause(fold)
                ),
                params![&self.identity[..], seq],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// The archive's newest sequence, 0 when it is empty: what a wake
    /// tells a watching client.
    pub fn archive_newest(&self) -> Result<u64, StoreError> {
        let newest: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(seq) FROM archive WHERE identity = ?1",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(newest.map_or(0, |seq| seq as u64))
    }

    /// The newest sequence a client is woken for: every entry's but a
    /// name's (1136), a petname's (1139) and a peer state's (1146), which
    /// ride along with the next pull.
    pub fn archive_newest_waking(&self) -> Result<u64, StoreError> {
        let newest: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(seq) FROM archive WHERE identity = ?1 AND kind NOT IN (?2, ?3, ?4)",
                params![
                    &self.identity[..],
                    i64::from(crate::home::KIND_NAME),
                    i64::from(crate::home::KIND_PETNAME),
                    i64::from(crate::home::KIND_PEERSTATE)
                ],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(newest.map_or(0, |seq| seq as u64))
    }

    /// SQLite's `data_version` of this connection: it moves when another
    /// connection, in this process or another, committed to the file since
    /// the last read, and never for this connection's own writes. The home
    /// node reads it to learn of an archive append by `loquax import` or
    /// another local tool (1164). One read takes the file's shared lock and
    /// reads its header: no table is touched.
    pub fn data_version(&self) -> Result<i64, StoreError> {
        self.conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(|e| self.failed(e))
    }

    /// The newest message the archive received from `peer`, packed as it
    /// came (1307 pass six): what that peer's client last said of itself.
    /// An LXMF message packs its destination, then its source.
    pub fn archive_newest_from(&self, peer: &[u8; 16]) -> Result<Option<Vec<u8>>, StoreError> {
        self.conn
            .query_row(
                "SELECT packed FROM archive WHERE identity = ?1 AND kind = ?2 \
                 AND substr(packed, 17, 16) = ?3 ORDER BY seq DESC LIMIT 1",
                params![
                    &self.identity[..],
                    i64::from(crate::home::KIND_RECEIVED),
                    &peer[..]
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| self.failed(e))
    }

    /// Forget every archived message of the conversation with `group`
    /// (1307 pass four, its admin deleted it): the members' lines to it,
    /// what the owner's devices sent it and what it sent the owner. The
    /// count removed. Devices that pulled them keep theirs until the
    /// group's last membership tells them it ended.
    pub fn archive_forget_group(&mut self, group: &[u8; 16]) -> Result<usize, StoreError> {
        // An LXMF message packs its destination, then its source.
        self.conn
            .execute(
                "DELETE FROM archive WHERE identity = ?1 AND ( \
                 (kind IN (?3, ?4) AND substr(packed, 1, 16) = ?2) \
                 OR (kind = ?5 AND substr(packed, 17, 16) = ?2))",
                params![
                    &self.identity[..],
                    &group[..],
                    i64::from(crate::home::KIND_GROUP),
                    i64::from(crate::home::KIND_SENT),
                    i64::from(crate::home::KIND_RECEIVED)
                ],
            )
            .map_err(|e| self.failed(e))
    }

    /// The peers the archive and the outbox hold a conversation with: the
    /// source of each received message, the destination of each sent one
    /// and of each message a client handed in. The home node archives the
    /// names of these alone ([`crate::home::relation`]).
    pub fn archive_peers(&self) -> Result<std::collections::BTreeSet<[u8; 16]>, StoreError> {
        let mut peers = std::collections::BTreeSet::new();
        // An LXMF message packs its destination, then its source.
        let mut statement = self
            .conn
            .prepare(
                "SELECT CASE kind WHEN ?2 THEN substr(packed, 17, 16) ELSE substr(packed, 1, 16) END \
                 FROM archive WHERE identity = ?1 AND kind IN (?2, ?3) \
                 UNION SELECT destination FROM home_outbox WHERE identity = ?1 \
                 UNION SELECT peer FROM conversations WHERE identity = ?1",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(
                params![
                    &self.identity[..],
                    i64::from(crate::home::KIND_RECEIVED),
                    i64::from(crate::home::KIND_SENT)
                ],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .map_err(|e| self.failed(e))?;
        for row in rows {
            let bytes = row.map_err(|e| self.failed(e))?;
            if let Ok(peer) = <[u8; 16]>::try_from(bytes.as_slice()) {
                peers.insert(peer);
            }
        }
        Ok(peers)
    }

    /// Whether this store is a home node's for our address: a device was
    /// admitted, or the archive holds an entry. An import writes the
    /// archive only then (1150).
    pub fn serves_as_home_node(&self) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 WHERE EXISTS (SELECT 1 FROM home_devices WHERE identity = ?1) \
                 OR EXISTS (SELECT 1 FROM archive WHERE identity = ?1)",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        Ok(found.is_some())
    }

    /// Append imported rows ([`crate::home::KIND_IMPORTED`]) in the order
    /// given, all or none, under the cap: how many were new to the archive.
    /// The same message twice is one entry, so a second import appends
    /// nothing.
    pub fn archive_imported(
        &mut self,
        entries: &[([u8; 32], i64, Vec<u8>)],
        cap_bytes: u64,
    ) -> Result<usize, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let mut added = 0;
        for (id, at, bytes) in entries {
            let seq = append_in(
                &tx,
                &identity,
                crate::home::KIND_IMPORTED,
                id,
                bytes,
                *at,
                cap_bytes,
            )
            .map_err(|e| failed(&path, e))?;
            added += usize::from(seq.is_some());
        }
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(added)
    }

    /// Every archive entry's id, kind and time, oldest first, without its
    /// bytes: what `loquax mailbox archive` prints, and what the N-home cells
    /// compare across homes (1305).
    pub fn archive_index(&self) -> Result<Vec<([u8; 32], u8, i64)>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, kind, at FROM archive WHERE identity = ?1 ORDER BY seq")
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                let id: Vec<u8> = row.get(0)?;
                let kind: i64 = row.get(1)?;
                Ok((
                    <[u8; 32]>::try_from(id.as_slice()).unwrap_or([0; 32]),
                    u8::try_from(kind).unwrap_or(u8::MAX),
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| self.failed(e))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| self.failed(e))
    }

    /// The archive after `cursor` as a move carries it (1281): each entry
    /// with its id, oldest first, a page of about `max_bytes` but at least
    /// one entry, and whether more follow, beside the oldest sequence held.
    pub fn archive_moving(
        &self,
        cursor: u64,
        max_bytes: usize,
    ) -> Result<(Vec<crate::home::wire::Moving>, bool, u64), StoreError> {
        let cursor = i64::try_from(cursor).unwrap_or(i64::MAX);
        let mut statement = self
            .conn
            .prepare(
                "SELECT seq, kind, id, at, packed FROM archive WHERE identity = ?1 AND seq > ?2 \
                 ORDER BY seq",
            )
            .map_err(|e| self.failed(e))?;
        let mut rows = statement
            .query(params![&self.identity[..], cursor])
            .map_err(|e| self.failed(e))?;
        let mut entries = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        while let Some(row) = rows.next().map_err(|e| self.failed(e))? {
            self.ops.add(Op::StoreRowsRead, 1);
            let packed: Vec<u8> = row.get(4).map_err(|e| self.failed(e))?;
            if !entries.is_empty() && bytes.saturating_add(packed.len()) > max_bytes {
                more = true;
                break;
            }
            bytes = bytes.saturating_add(packed.len());
            let seq: i64 = row.get(0).map_err(|e| self.failed(e))?;
            let kind: i64 = row.get(1).map_err(|e| self.failed(e))?;
            let id: Vec<u8> = row.get(2).map_err(|e| self.failed(e))?;
            entries.push(crate::home::wire::Moving {
                seq: seq as u64,
                kind: u8::try_from(kind).unwrap_or(u8::MAX),
                id: <[u8; 32]>::try_from(id.as_slice()).unwrap_or([0; 32]),
                at: row.get(3).map_err(|e| self.failed(e))?,
                packed,
            });
        }
        let first: Option<i64> = self
            .conn
            .query_row(
                "SELECT MIN(seq) FROM archive WHERE identity = ?1",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok((entries, more, first.map_or(0, |seq| seq as u64)))
    }

    /// Append the entries a move brought (1281), in their order, each of
    /// its own kind, id and time, all or none, under the cap: how many were
    /// new to this archive.
    pub fn archive_moved_in(
        &mut self,
        entries: &[crate::home::wire::Moving],
        cap_bytes: u64,
    ) -> Result<usize, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let mut added = 0;
        for entry in entries {
            let seq = append_in(
                &tx,
                &identity,
                entry.kind,
                &entry.id,
                &entry.packed,
                entry.at,
                cap_bytes,
            )
            .map_err(|e| failed(&path, e))?;
            added += usize::from(seq.is_some());
        }
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(added)
    }

    /// How many bytes of packed messages the archive holds.
    pub fn archive_bytes(&self) -> Result<u64, StoreError> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(LENGTH(packed)), 0) FROM archive WHERE identity = ?1",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(total.max(0) as u64)
    }

    /// Open one single-use admission token for a device to be called `name`.
    pub fn issue_token(&mut self, token: &[u8; 16], name: &str, at: i64) -> Result<(), StoreError> {
        self.conn
            .execute(
                "INSERT INTO home_tokens(identity, token, name, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![&self.identity[..], &token[..], name, at],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// Redeem `token` for `device`, in one transaction: the token goes and
    /// the device is admitted under the token's name, or nothing changes.
    /// A device admitted again keeps one row, with the new name and time.
    /// A token issued `ttl_secs` or longer before `at` has expired: it is
    /// removed and refused as an unknown one is.
    pub fn redeem_token(
        &mut self,
        token: &[u8; 16],
        device: &[u8; 16],
        at: i64,
        ttl_secs: i64,
    ) -> Result<Redeemed, StoreError> {
        self.redeem_token_as(token, device, "", at, ttl_secs)
    }

    /// [`Store::redeem_token`], with the name the device brought in its
    /// join: a token issued without a name (a phone's "Link a device",
    /// 1264, which cannot know the other device's name) admits it under
    /// that one. A token with a name keeps it, as the person who issued it
    /// chose it.
    pub fn redeem_token_as(
        &mut self,
        token: &[u8; 16],
        device: &[u8; 16],
        brought: &str,
        at: i64,
        ttl_secs: i64,
    ) -> Result<Redeemed, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let open: Option<(String, i64)> = tx
            .query_row(
                "SELECT name, created_at FROM home_tokens WHERE identity = ?1 AND token = ?2",
                params![&identity[..], &token[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| failed(&path, e))?;
        let Some((mut name, created_at)) = open else {
            return Ok(Redeemed::Unknown);
        };
        if name.trim().is_empty() {
            name = match brought.trim() {
                "" => "device".to_string(),
                brought => brought.to_string(),
            };
        }
        tx.execute(
            "DELETE FROM home_tokens WHERE identity = ?1 AND token = ?2",
            params![&identity[..], &token[..]],
        )
        .map_err(|e| failed(&path, e))?;
        if at.saturating_sub(created_at) >= ttl_secs {
            tx.commit().map_err(|e| failed(&path, e))?;
            return Ok(Redeemed::Expired { name });
        }
        tx.execute(
            "INSERT OR REPLACE INTO home_devices(identity, device, name, admitted_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![&identity[..], &device[..], &name, at],
        )
        .map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(Redeemed::Admitted { name })
    }

    /// Whether `device` is admitted now. Every request asks again.
    pub fn is_admitted(&self, device: &[u8; 16]) -> Result<bool, StoreError> {
        self.conn
            .query_row(
                "SELECT 1 FROM home_devices WHERE identity = ?1 AND device = ?2",
                params![&self.identity[..], &device[..]],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(|e| self.failed(e))
    }

    /// Every admitted device, the earliest admitted first.
    pub fn devices(&self) -> Result<Vec<Device>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT device, name, admitted_at, last_seen FROM home_devices \
                 WHERE identity = ?1 ORDER BY admitted_at, name",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .map(|(device, name, admitted_at, last_seen)| {
                Ok(Device {
                    device: self.address(&device)?,
                    name,
                    admitted_at,
                    last_seen,
                })
            })
            .collect()
    }

    /// `device` asked at `at` (1642): its `last_seen` moves there, at most
    /// once a [`SEEN_EVERY_SECS`], so a busy link does not write on every
    /// request. A device that is not admitted changes nothing.
    pub fn device_seen(&self, device: &[u8; 16], at: i64) -> Result<(), StoreError> {
        self.conn
            .execute(
                "UPDATE home_devices SET last_seen = ?3 WHERE identity = ?1 AND device = ?2 \
                 AND (last_seen IS NULL OR last_seen <= ?3 - ?4)",
                params![&self.identity[..], &device[..], at, SEEN_EVERY_SECS],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// The admission tokens still open at `now`. Expired ones are removed
    /// first, so the list is what a join could still redeem.
    pub fn open_tokens(&mut self, now: i64, ttl_secs: i64) -> Result<Vec<OpenToken>, StoreError> {
        self.conn
            .execute(
                "DELETE FROM home_tokens WHERE identity = ?1 AND ?2 - created_at >= ?3",
                params![&self.identity[..], now, ttl_secs],
            )
            .map_err(|e| self.failed(e))?;
        self.tokens()
    }

    /// Every token issued and not redeemed, expired or not.
    fn tokens(&self) -> Result<Vec<OpenToken>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT token, name, created_at FROM home_tokens WHERE identity = ?1 \
                 ORDER BY created_at, name",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .map(|(token, name, created_at)| {
                Ok(OpenToken {
                    token: self.address(&token)?,
                    name,
                    created_at,
                })
            })
            .collect()
    }

    /// Withdraw the token whose hex starts with `prefix` (`loquax home
    /// revoke-token`): at least four hex characters, as `home revoke` takes
    /// a device's hash, and only when one token matches.
    pub fn revoke_token(&mut self, prefix: &str) -> Result<TokenRevoked, StoreError> {
        let lower = prefix.trim().to_ascii_lowercase();
        if lower.len() < 4 || !lower.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(TokenRevoked::TooShort);
        }
        let mut matched: Vec<OpenToken> = self
            .tokens()?
            .into_iter()
            .filter(|token| crate::address::to_hex(&token.token).starts_with(&lower))
            .collect();
        match matched.len() {
            0 => Ok(TokenRevoked::None),
            1 => {
                let token = matched.remove(0);
                self.conn
                    .execute(
                        "DELETE FROM home_tokens WHERE identity = ?1 AND token = ?2",
                        params![&self.identity[..], &token.token[..]],
                    )
                    .map_err(|e| self.failed(e))?;
                Ok(TokenRevoked::One(token))
            }
            _ => Ok(TokenRevoked::Ambiguous(matched)),
        }
    }

    /// The message an earlier `/send` with `client_id` made, if one did.
    pub fn outbox_by_client(&self, client_id: &[u8; 16]) -> Result<Option<[u8; 32]>, StoreError> {
        let id: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT id FROM home_outbox WHERE identity = ?1 AND client_id = ?2",
                params![&self.identity[..], &client_id[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        id.map(|id| {
            <[u8; 32]>::try_from(id.as_slice())
                .map_err(|_| self.failed("a message id is not 32 bytes"))
        })
        .transpose()
    }

    /// Put one message a client handed in into the outbox, and its sent
    /// entry into the archive, in one transaction. A `client_id` seen before
    /// changes nothing and names the message it made then.
    #[allow(clippy::too_many_arguments)]
    pub fn outbox_accept(
        &mut self,
        client_id: &[u8; 16],
        id: &[u8; 32],
        destination: &[u8; 16],
        packed: &[u8],
        at: i64,
        cap_bytes: u64,
    ) -> Result<Accepted, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let earlier: Option<Vec<u8>> = tx
            .query_row(
                "SELECT id FROM home_outbox WHERE identity = ?1 AND client_id = ?2",
                params![&identity[..], &client_id[..]],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| failed(&path, e))?;
        if let Some(earlier) = earlier {
            let id = <[u8; 32]>::try_from(earlier.as_slice())
                .map_err(|_| failed(&path, "a message id is not 32 bytes"))?;
            return Ok(Accepted::Again { id });
        }
        tx.execute(
            "INSERT INTO home_outbox(identity, client_id, id, destination, packed, created_at, \
             state, propagated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0)",
            params![
                &identity[..],
                &client_id[..],
                &id[..],
                &destination[..],
                packed,
                at
            ],
        )
        .map_err(|e| failed(&path, e))?;
        let seq = append_in(
            &tx,
            &identity,
            crate::home::KIND_SENT,
            id,
            packed,
            at,
            cap_bytes,
        )
        .map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(Accepted::New { seq })
    }

    /// Every outbox message still retried, the oldest first.
    pub fn outbox_open(&self) -> Result<Vec<Outgoing>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, destination, packed, created_at, propagated FROM home_outbox \
                 WHERE identity = ?1 AND state = 0 ORDER BY created_at, id",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .map(|(id, destination, packed, created_at, propagated)| {
                Ok(Outgoing {
                    id: <[u8; 32]>::try_from(id.as_slice())
                        .map_err(|_| self.failed("a message id is not 32 bytes"))?,
                    destination: self.address(&destination)?,
                    packed,
                    created_at,
                    propagated: propagated != 0,
                })
            })
            .collect()
    }

    /// Record one state change of an outbox message: its row and the archive
    /// entry clients see it by ([`Change::kind`], the message id as its
    /// bytes), in one transaction.
    pub fn outbox_record(
        &mut self,
        id: &[u8; 32],
        change: Change,
        at: i64,
        cap_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let update = match change {
            Change::Propagated => {
                Some("UPDATE home_outbox SET propagated = 1 WHERE identity = ?1 AND id = ?2")
            }
            Change::Delivered => {
                Some("UPDATE home_outbox SET state = 1 WHERE identity = ?1 AND id = ?2")
            }
            Change::Ended { .. } => {
                Some("UPDATE home_outbox SET state = 2 WHERE identity = ?1 AND id = ?2")
            }
            // Still open: the row keeps no leg, so a restart tries it anew.
            Change::Stalled => None,
        };
        if let Some(update) = update {
            tx.execute(update, params![&identity[..], &id[..]])
                .map_err(|e| failed(&path, e))?;
        }
        // A state entry's bytes are the message id, which is all a client
        // needs to find the message the change is of.
        let seq = append_in(&tx, &identity, change.kind(), id, &id[..], at, cap_bytes)
            .map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(seq)
    }

    /// Remove the device `which` names: its name exactly, or its hash in
    /// hex, whole or a prefix of at least four characters. More than one
    /// match removes nothing.
    pub fn revoke(&mut self, which: &str) -> Result<Revoked, StoreError> {
        let which = which.trim();
        let lower = which.to_ascii_lowercase();
        let hex_prefix = lower.len() >= 4 && lower.chars().all(|c| c.is_ascii_hexdigit());
        let matched: Vec<Device> = self
            .devices()?
            .into_iter()
            .filter(|device| {
                device.name == which
                    || (hex_prefix && crate::address::to_hex(&device.device).starts_with(&lower))
            })
            .collect();
        match matched.len() {
            0 => Ok(Revoked::None),
            1 => {
                let device = matched
                    .into_iter()
                    .next()
                    .ok_or_else(|| self.failed("a matched device vanished between two reads"))?;
                self.conn
                    .execute(
                        "DELETE FROM home_devices WHERE identity = ?1 AND device = ?2",
                        params![&self.identity[..], &device.device[..]],
                    )
                    .map_err(|e| self.failed(e))?;
                Ok(Revoked::One(device))
            }
            _ => Ok(Revoked::Ambiguous(matched)),
        }
    }
}

/// One message an import brings (`crate::import`), as the store keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRow {
    /// The LXMF message id the source names it by.
    pub id: [u8; 32],
    /// Written by the address the history belongs to.
    pub outbound: bool,
    /// The sender's clock, where the source has it.
    pub timestamp: Option<f64>,
    /// When the source app wrote or received it, unix seconds.
    pub received_at: Option<i64>,
    pub title: Vec<u8>,
    pub content: String,
    pub fields: Vec<leviculum_lxmf::Field>,
    /// Where a sent message stood in the source app.
    pub delivery: Option<DeliveryState>,
    /// Our own verdict: [`Signature::Verified`] only where loquax checked
    /// the signature itself.
    pub signature: Signature,
    /// What the source app said of the signature, shown as its word.
    pub origin_verdict: Option<bool>,
    /// The sender and the signed bytes, kept while the sender's key is not
    /// known, as a received message's are (`unverified`).
    pub held: Option<([u8; 16], Vec<u8>)>,
}

/// What importing one conversation did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportCounts {
    /// Rows written.
    pub new: usize,
    /// Rows the store already held under the same id and direction.
    pub held: usize,
}

/// One message as `loquax export` writes it out.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportRow {
    pub peer: [u8; 16],
    pub row: ImportRow,
}

/// The import's and the export's half of the store (`crate::import`,
/// `crate::export`).
impl Store {
    /// Whether the store holds a message with this id and direction for our
    /// address: the import's dedup rule, 1070's.
    pub fn holds(&self, id: &[u8; 32], outbound: bool) -> Result<bool, StoreError> {
        holds_in(&self.conn, &self.identity, id, outbound).map_err(|e| self.failed(e))
    }

    /// How many messages the store holds for our address.
    pub fn message_count(&self) -> Result<usize, StoreError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE identity = ?1",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(count.max(0) as usize)
    }

    /// Write one conversation's imported messages, all or none. The rows
    /// the store does not hold yet join the conversation's imported block,
    /// and the whole block is laid out again below the store's lowest key in
    /// time order, so a second import that brings older messages still
    /// shows them first and loquax's own rows keep their keys. Each row's
    /// ledger says where it came from, and what the source said of its
    /// signature, in the source's name ([`origin_name`]).
    pub fn import_conversation(
        &mut self,
        peer: [u8; 16],
        origin: &str,
        rows: &[ImportRow],
    ) -> Result<ImportCounts, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let counts = import_in(&tx, &identity, peer, origin, rows).map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(counts)
    }

    /// Every message of our address, oldest first in each conversation, with
    /// the signed bytes the store still holds for one: what `loquax export`
    /// writes.
    pub fn export_rows(&self) -> Result<Vec<ExportRow>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT m.conversation, m.id, m.direction, m.state, m.verification, \
                 m.timestamp, m.received_at, m.title, m.content, m.fields, m.origin_verdict, \
                 u.source, u.packed FROM messages m LEFT JOIN unverified u \
                 ON u.identity = m.identity AND u.id = m.id AND m.direction = 0 \
                 WHERE m.identity = ?1 ORDER BY m.conversation, m.seq",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                let peer: Vec<u8> = row.get(0)?;
                let id: Option<Vec<u8>> = row.get(1)?;
                let source: Option<Vec<u8>> = row.get(11)?;
                let packed: Option<Vec<u8>> = row.get(12)?;
                Ok((
                    peer,
                    id,
                    ImportRow {
                        id: [0; 32],
                        outbound: row.get::<_, i64>(2)? == 1,
                        delivery: row.get::<_, Option<i64>>(3)?.and_then(state_of),
                        signature: signature_of(row.get(4)?),
                        timestamp: row.get(5)?,
                        received_at: row.get(6)?,
                        title: row.get::<_, Option<Vec<u8>>>(7)?.unwrap_or_default(),
                        content: row.get(8)?,
                        fields: row
                            .get::<_, Option<Vec<u8>>>(9)?
                            .and_then(|packed| unpack_fields(&packed))
                            .unwrap_or_default(),
                        origin_verdict: row.get(10)?,
                        held: source
                            .and_then(|s| <[u8; 16]>::try_from(s.as_slice()).ok())
                            .zip(packed),
                    },
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map(|rows| self.read(rows))
            .map_err(|e| self.failed(e))?;
        let mut out = Vec::with_capacity(rows.len());
        for (peer, id, mut row) in rows {
            // A message with no id never reached the router: nothing names
            // it, and the export gives it the id of the bytes it writes.
            row.id = id
                .and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok())
                .unwrap_or([0; 32]);
            out.push(ExportRow {
                peer: self.address(&peer)?,
                row,
            });
        }
        Ok(out)
    }
}

/// An origin tag's name as the ledger says it.
pub fn origin_name(tag: &str) -> &str {
    match tag {
        "nomadnet" => "NomadNet",
        "loquax" => "Loquax",
        "ratspeak" => "Ratspeak",
        other => other,
    }
}

/// Whether the app an origin tag names keeps no signed bytes, so an
/// imported row of it can never be checked: Ratspeak (1150).
pub fn origin_keeps_no_signature(tag: &str) -> bool {
    tag == "ratspeak"
}

fn holds_in(
    conn: &Connection,
    identity: &[u8; 16],
    id: &[u8; 32],
    outbound: bool,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM messages WHERE id = ?1 AND identity = ?2 AND direction = ?3 LIMIT 1",
        params![&id[..], &identity[..], i64::from(outbound)],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

/// [`Store::import_conversation`] inside `tx`.
fn import_in(
    tx: &rusqlite::Transaction<'_>,
    identity: &[u8; 16],
    peer: [u8; 16],
    origin: &str,
    rows: &[ImportRow],
) -> rusqlite::Result<ImportCounts> {
    let mut counts = ImportCounts::default();
    let mut fresh = Vec::new();
    for row in rows {
        let duplicate = fresh
            .iter()
            .any(|f: &ImportRow| f.id == row.id && f.outbound == row.outbound);
        if duplicate || holds_in(tx, identity, &row.id, row.outbound)? {
            counts.held += 1;
        } else {
            fresh.push(row.clone());
        }
    }
    counts.new = fresh.len();
    if fresh.is_empty() {
        return Ok(counts);
    }
    // The conversation's imported block as it stands, read whole, then
    // deleted and written again with the new rows in time order. Deleting
    // and inserting rather than renumbering keeps the text index, whose
    // rowid is the seq, in step through its triggers.
    let mut block = {
        let mut statement = tx.prepare(
            "SELECT id, direction, state, verification, timestamp, received_at, title, \
             content, fields, origin_verdict, origin, read_at FROM messages \
             WHERE identity = ?1 AND conversation = ?2 AND origin IS NOT NULL",
        )?;
        let rows = statement
            .query_map(params![&identity[..], &peer[..]], |row| {
                let id: Vec<u8> = row.get(0)?;
                let read: Option<i64> = row.get(11)?;
                Ok((
                    ImportRow {
                        id: <[u8; 32]>::try_from(id.as_slice()).unwrap_or([0; 32]),
                        outbound: row.get::<_, i64>(1)? == 1,
                        // With its time (1506): a receipt that read an
                        // imported message since keeps it through the
                        // block's new layout.
                        delivery: row
                            .get::<_, Option<i64>>(2)?
                            .and_then(|code| delivery_of(code, read)),
                        signature: signature_of(row.get(3)?),
                        timestamp: row.get(4)?,
                        received_at: row.get(5)?,
                        title: row.get::<_, Option<Vec<u8>>>(6)?.unwrap_or_default(),
                        content: row.get(7)?,
                        fields: row
                            .get::<_, Option<Vec<u8>>>(8)?
                            .and_then(|packed| unpack_fields(&packed))
                            .unwrap_or_default(),
                        origin_verdict: row.get(9)?,
                        held: None,
                    },
                    row.get::<_, String>(10)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    tx.execute(
        "DELETE FROM messages WHERE identity = ?1 AND conversation = ?2 AND origin IS NOT NULL",
        params![&identity[..], &peer[..]],
    )?;
    block.extend(fresh.into_iter().map(|row| (row, origin.to_string())));
    // The source's time order: the sender's clock, else when the source
    // wrote it down; the id breaks a tie, so a re-run lays it out the same.
    let when = |row: &ImportRow| {
        row.timestamp
            .or(row.received_at.map(|secs| secs as f64))
            .unwrap_or(0.0)
    };
    block.sort_by(|(a, _), (b, _)| when(a).total_cmp(&when(b)).then(a.id.cmp(&b.id)));
    let lowest: i64 = tx.query_row("SELECT COALESCE(MIN(seq), 1) FROM messages", [], |row| {
        row.get(0)
    })?;
    let top = lowest.min(1) - 1;
    let first = top - (block.len() as i64 - 1);
    tx.execute(
        "INSERT INTO conversations(identity, peer) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
        params![&identity[..], &peer[..]],
    )?;
    for (offset, (row, origin)) in block.iter().enumerate() {
        let seq = first + offset as i64;
        tx.execute(
            "INSERT INTO messages(seq, identity, conversation, id, direction, state, \
             verification, timestamp, received_at, size, title, content, fields, origin, \
             origin_verdict, read_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                seq,
                &identity[..],
                &peer[..],
                &row.id[..],
                i64::from(row.outbound),
                row.delivery.map(state_code),
                signature_code(row.signature),
                row.timestamp,
                row.received_at,
                row.content.len() as i64,
                (!row.title.is_empty()).then_some(&row.title[..]),
                row.content,
                (!row.fields.is_empty()).then(|| pack_fields(&row.fields)),
                origin,
                row.origin_verdict,
                read_at(row.delivery),
            ],
        )?;
        let name = origin_name(origin);
        let mut lines = vec![format!("imported from {name}")];
        if origin_keeps_no_signature(origin) {
            lines.push(format!("imported, no signature: {name} keeps none"));
        }
        if let Some(verdict) = row.origin_verdict {
            lines.push(match verdict {
                true => format!("{name} said: signature verified"),
                false => format!("{name} said: signature not verified"),
            });
        }
        for (line, what) in lines.iter().enumerate() {
            tx.execute(
                "INSERT INTO ledger(message, line, at, utc_offset, what) \
                 VALUES (?1, ?2, ?3, NULL, ?4)",
                params![seq, line as i64, row.received_at, what],
            )?;
        }
        if let Some((source, packed)) = &row.held {
            tx.execute(
                "INSERT OR IGNORE INTO unverified(identity, id, source, packed) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![&identity[..], &row.id[..], &source[..], packed],
            )?;
        }
    }
    Ok(counts)
}

/// One archive append inside `tx`, with the cap's eviction: see
/// [`Store::archive_append`].
fn append_in(
    tx: &rusqlite::Transaction<'_>,
    identity: &[u8; 16],
    kind: u8,
    id: &[u8; 32],
    packed: &[u8],
    at: i64,
    cap_bytes: u64,
) -> rusqlite::Result<Option<u64>> {
    let added = tx.execute(
        "INSERT OR IGNORE INTO archive(identity, kind, id, at, packed) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![&identity[..], i64::from(kind), &id[..], at, packed],
    )?;
    if added == 0 {
        return Ok(None);
    }
    let seq = tx.last_insert_rowid();
    let mut total: i64 = tx.query_row(
        "SELECT COALESCE(SUM(LENGTH(packed)), 0) FROM archive WHERE identity = ?1",
        params![&identity[..]],
        |row| row.get(0),
    )?;
    let cap = i64::try_from(cap_bytes).unwrap_or(i64::MAX);
    while total > cap {
        let oldest: Option<(i64, i64)> = tx
            .query_row(
                "SELECT seq, LENGTH(packed) FROM archive WHERE identity = ?1 AND seq < ?2 \
                 ORDER BY seq LIMIT 1",
                params![&identity[..], seq],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((victim, size)) = oldest else {
            break;
        };
        tx.execute("DELETE FROM archive WHERE seq = ?1", params![victim])?;
        total -= size;
    }
    Ok(Some(seq as u64))
}

/// The archive query's condition for a device that folds as `fold`
/// (1439), built from the wire's own constants so the home serves exactly
/// what `Fold::keeps` says; empty for a device that keeps everything.
fn fold_clause(fold: crate::home::wire::Fold) -> String {
    use crate::home::wire::{Fold, BOARD_ENTRY_MAX, BOARD_KINDS};
    match fold {
        Fold::All => String::new(),
        Fold::Board => {
            let kinds: Vec<String> = BOARD_KINDS.iter().map(u8::to_string).collect();
            format!(
                " AND kind IN ({}) AND length(packed) <= {BOARD_ENTRY_MAX}",
                kinds.join(",")
            )
        }
    }
}

fn failed(path: &Path, error: impl fmt::Display) -> StoreError {
    let detail = error.to_string();
    // SQLite's words for a file that is not whole, wherever they surface.
    let damaged = ["malformed", "not a database", "corrupt"];
    match damaged.iter().any(|word| detail.contains(word)) {
        true => StoreError::Corrupt {
            path: path.to_path_buf(),
            detail,
        },
        false => StoreError::Failed {
            path: path.to_path_buf(),
            detail,
        },
    }
}

fn read_pn_asks(
    conn: &Connection,
    identity: &[u8; 16],
) -> rusqlite::Result<crate::pn_status::AskerState> {
    let mut state = crate::pn_status::AskerState::default();
    let mut asks = conn.prepare(
        "SELECT transient, message, node, uploaded_at FROM pn_asks WHERE identity = ?1 \
         ORDER BY uploaded_at, transient",
    )?;
    let rows = asks.query_map(params![&identity[..]], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (transient, message, node, uploaded_at) = row?;
        // A row of another length is no id: passed over.
        if let (Ok(transient_id), Ok(message_id), Ok(node)) = (
            <[u8; 32]>::try_from(transient.as_slice()),
            <[u8; 32]>::try_from(message.as_slice()),
            <[u8; 16]>::try_from(node.as_slice()),
        ) {
            state.open.push(crate::pn_status::Open {
                transient_id,
                message_id,
                node,
                uploaded_at,
            });
        }
    }
    let mut nodes =
        conn.prepare("SELECT node, version FROM pn_nodes WHERE identity = ?1 ORDER BY node")?;
    let rows = nodes.query_map(params![&identity[..]], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (node, version) = row?;
        if let Ok(node) = <[u8; 16]>::try_from(node.as_slice()) {
            state
                .answering
                .push((node, u64::try_from(version).unwrap_or(0)));
        }
    }
    Ok(state)
}

fn write_pn_asks(
    tx: &rusqlite::Transaction<'_>,
    identity: &[u8; 16],
    state: &crate::pn_status::AskerState,
) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM pn_asks WHERE identity = ?1",
        params![&identity[..]],
    )?;
    tx.execute(
        "DELETE FROM pn_nodes WHERE identity = ?1",
        params![&identity[..]],
    )?;
    for open in &state.open {
        tx.execute(
            "INSERT OR REPLACE INTO pn_asks(identity, transient, message, node, uploaded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &identity[..],
                &open.transient_id[..],
                &open.message_id[..],
                &open.node[..],
                open.uploaded_at
            ],
        )?;
    }
    for (node, version) in &state.answering {
        tx.execute(
            "INSERT OR REPLACE INTO pn_nodes(identity, node, version) VALUES (?1, ?2, ?3)",
            params![
                &identity[..],
                &node[..],
                i64::try_from(*version).unwrap_or(i64::MAX)
            ],
        )?;
    }
    Ok(())
}

fn apply(
    tx: &rusqlite::Transaction<'_>,
    identity: &[u8; 16],
    write: &StoreWrite,
) -> rusqlite::Result<()> {
    match write {
        StoreWrite::Message { peer, message } => {
            let key = seq_of(message.activity);
            // One row per received message: a second copy under another key
            // is the same message, arrived again, and changes nothing.
            if let (false, Some(id)) = (message.outbound, &message.id) {
                if received_key(tx, identity, id, Some(key))?.is_some() {
                    return Ok(());
                }
            }
            tx.execute(
                "INSERT INTO conversations(identity, peer) VALUES (?1, ?2) \
                 ON CONFLICT DO NOTHING",
                params![&identity[..], &peer[..]],
            )?;
            let (secs, offset) = split(message.at);
            tx.execute(
                "INSERT INTO messages(seq, identity, conversation, id, direction, state, \
                 unproven, verification, last_leg, received_at, utc_offset, size, content, \
                 fields, read_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
                 ON CONFLICT(seq) DO UPDATE SET id = excluded.id, state = excluded.state, \
                 unproven = excluded.unproven, verification = excluded.verification, \
                 last_leg = excluded.last_leg, read_at = excluded.read_at",
                params![
                    key,
                    &identity[..],
                    &peer[..],
                    message.id.as_ref().map(|id| &id[..]),
                    i64::from(message.outbound),
                    message.delivery.map(state_code),
                    message.unproven,
                    signature_code(message.signature),
                    message.last_leg.map(leg_code),
                    secs,
                    offset,
                    message.size as i64,
                    message.body,
                    (!message.fields.is_empty()).then(|| pack_fields(&message.fields)),
                    read_at(message.delivery),
                ],
            )?;
            // The ledger only grows: the lines past what the store has.
            let have: i64 = tx.query_row(
                "SELECT COUNT(*) FROM ledger WHERE message = ?1",
                params![key],
                |row| row.get(0),
            )?;
            for (line, entry) in message.ledger.iter().enumerate().skip(have as usize) {
                let (at, offset) = split(entry.at);
                tx.execute(
                    "INSERT INTO ledger(message, line, at, utc_offset, what) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![key, line as i64, at, offset, entry.what],
                )?;
            }
        }
        StoreWrite::Conversation {
            peer,
            pinned,
            unread,
            flags,
        } => {
            tx.execute(
                "INSERT INTO conversations(identity, peer, pinned, unread, muted_until, \
                 archived, verified, receipts) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                 ON CONFLICT(identity, peer) DO UPDATE SET \
                 pinned = excluded.pinned, unread = excluded.unread, \
                 muted_until = excluded.muted_until, archived = excluded.archived, \
                 verified = excluded.verified, receipts = excluded.receipts",
                params![
                    &identity[..],
                    &peer[..],
                    pinned,
                    *unread as i64,
                    flags.muted_until,
                    flags.archived,
                    flags.verified,
                    flags.receipts
                ],
            )?;
        }
        StoreWrite::ForgetMessage { key } => {
            tx.execute(
                "DELETE FROM messages WHERE seq = ?1 AND identity = ?2",
                params![seq_of(*key), &identity[..]],
            )?;
        }
        StoreWrite::ForgetId { id } => {
            tx.execute(
                "DELETE FROM messages WHERE identity = ?1 AND id = ?2",
                params![&identity[..], &id[..]],
            )?;
        }
        StoreWrite::ForgetConversation { peer } => {
            tx.execute(
                "DELETE FROM messages WHERE identity = ?1 AND conversation = ?2",
                params![&identity[..], &peer[..]],
            )?;
            tx.execute(
                "DELETE FROM drafts WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
            tx.execute(
                "DELETE FROM conversations WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
            tx.execute(
                "DELETE FROM shares WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
            tx.execute(
                "DELETE FROM positions WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
            tx.execute(
                "DELETE FROM client_groups WHERE identity = ?1 AND address = ?2",
                params![&identity[..], &peer[..]],
            )?;
        }
        StoreWrite::Listing { peer, listing } => {
            tx.execute(
                "INSERT INTO conversations(identity, peer, listing) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(identity, peer) DO UPDATE SET listing = excluded.listing",
                params![&identity[..], &peer[..], listing.encode()],
            )?;
        }
        StoreWrite::ReadThrough { id } => {
            // Another device read through `id` and the screen does not
            // hold it (1490): the rows say it instead. Everything
            // received after it stays unread; the kept listing's badge
            // follows where it can, and heals from a page otherwise.
            let found: Option<(Vec<u8>, i64)> = tx
                .query_row(
                    "SELECT conversation, seq FROM messages \
                     WHERE identity = ?1 AND id = ?2 AND direction = 0",
                    params![&identity[..], &id[..]],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((peer, seq)) = found {
                let after: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM messages WHERE identity = ?1 \
                     AND conversation = ?2 AND direction = 0 AND seq > ?3",
                    params![&identity[..], &peer[..], seq],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "UPDATE conversations SET unread = MIN(unread, ?3) \
                     WHERE identity = ?1 AND peer = ?2",
                    params![&identity[..], &peer[..], after],
                )?;
                let blob: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT listing FROM conversations \
                         WHERE identity = ?1 AND peer = ?2",
                        params![&identity[..], &peer[..]],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(listing) = blob.as_deref().and_then(Listing::decode) {
                    let patched = match after {
                        0 => {
                            let mut read = listing;
                            read.unread_rows = 0;
                            Some(read.encode())
                        }
                        // Reports among the rest are not told apart
                        // here: the next fold, or a page at start,
                        // rewrites the row.
                        _ => None,
                    };
                    tx.execute(
                        "UPDATE conversations SET listing = ?3 \
                         WHERE identity = ?1 AND peer = ?2",
                        params![&identity[..], &peer[..], patched],
                    )?;
                }
            }
        }
        StoreWrite::PnAsks(state) => write_pn_asks(tx, identity, state)?,
        StoreWrite::OwedPeerStates(owed) => {
            tx.execute(
                "DELETE FROM owed_peer_states WHERE identity = ?1",
                params![&identity[..]],
            )?;
            for (peer, state) in owed {
                tx.execute(
                    "INSERT INTO owed_peer_states(identity, peer, blocked, pinned, archived, \
                     muted_until) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        &identity[..],
                        &peer[..],
                        state.blocked,
                        state.pinned,
                        state.archived,
                        state.muted_until
                    ],
                )?;
            }
        }
        StoreWrite::OwedPetnames(owed) => {
            tx.execute(
                "DELETE FROM owed_petnames WHERE identity = ?1",
                params![&identity[..]],
            )?;
            for (peer, petname) in owed {
                tx.execute(
                    "INSERT INTO owed_petnames(identity, peer, petname) VALUES (?1, ?2, ?3)",
                    params![&identity[..], &peer[..], petname],
                )?;
            }
        }
        StoreWrite::Group(group) => {
            tx.execute(
                "INSERT INTO client_groups(identity, address, name, admin_name, inviter, ours, \
                 epoch, standing, members) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(identity, address) DO UPDATE SET name = excluded.name, \
                 admin_name = excluded.admin_name, inviter = excluded.inviter, \
                 ours = excluded.ours, epoch = excluded.epoch, standing = excluded.standing, \
                 members = excluded.members",
                params![
                    &identity[..],
                    &group.address[..],
                    group.name,
                    group.admin_name,
                    group.inviter.as_ref().map(|i| &i[..]),
                    group.ours,
                    i64::try_from(group.epoch).unwrap_or(i64::MAX),
                    match group.standing {
                        Standing::Invited => 0,
                        Standing::Joined => 1,
                        Standing::Left => 2,
                        Standing::Ended => 3,
                    },
                    group.members_bytes()
                ],
            )?;
        }
        StoreWrite::Draft { peer, draft } if draft.is_empty() => {
            tx.execute(
                "DELETE FROM drafts WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
        }
        // Beside its conversation, so a conversation with nothing in it but
        // a draft comes back at start.
        StoreWrite::Draft { peer, draft } => {
            tx.execute(
                "INSERT INTO conversations(identity, peer) VALUES (?1, ?2) \
                 ON CONFLICT DO NOTHING",
                params![&identity[..], &peer[..]],
            )?;
            let attachments = serde_json::to_string(&draft.attachments)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            tx.execute(
                "INSERT INTO drafts(identity, peer, body, reply, attachments) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(identity, peer) DO UPDATE SET body = excluded.body, \
                 reply = excluded.reply, attachments = excluded.attachments",
                params![
                    &identity[..],
                    &peer[..],
                    draft.text,
                    draft.reply.as_ref().map(|id| &id[..]),
                    attachments,
                ],
            )?;
        }
        StoreWrite::Share { peer, share: None } => {
            tx.execute(
                "DELETE FROM shares WHERE identity = ?1 AND peer = ?2",
                params![&identity[..], &peer[..]],
            )?;
        }
        StoreWrite::Share {
            peer,
            share: Some(share),
        } => {
            let last = share.last.as_ref();
            tx.execute(
                "INSERT INTO shares(identity, peer, span, started_at, until, last_lat, last_lon, \
                 last_alt, last_accuracy, last_fix_at, last_at, last_moved) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT(identity, peer) DO UPDATE SET span = excluded.span, \
                 started_at = excluded.started_at, until = excluded.until, \
                 last_lat = excluded.last_lat, last_lon = excluded.last_lon, \
                 last_alt = excluded.last_alt, last_accuracy = excluded.last_accuracy, \
                 last_fix_at = excluded.last_fix_at, last_at = excluded.last_at, \
                 last_moved = excluded.last_moved",
                params![
                    &identity[..],
                    &peer[..],
                    share.span.word(),
                    share.started_at,
                    share.until,
                    last.map(|l| l.fix.latitude_e6),
                    last.map(|l| l.fix.longitude_e6),
                    last.map(|l| l.fix.altitude_e2),
                    last.map(|l| l.fix.accuracy_e2),
                    last.map(|l| l.fix.last_update),
                    last.map(|l| l.at),
                    last.is_some_and(|l| l.moved),
                ],
            )?;
        }
        StoreWrite::Position(fix) => {
            let l = &fix.location;
            tx.execute(
                // One row a peer and fix time: the first to arrive stays,
                // except that a fix a message carried takes the place of
                // the same fix announced, so the row names its message.
                "INSERT INTO positions(identity, peer, via, message, fix_time, \
                 received_at, lat_e6, lon_e6, alt_e2, speed_e2, bearing_e2, accuracy_e2, \
                 telemetry, source) \
                 VALUES (?1, ?2, ?3, \
                 (SELECT seq FROM messages WHERE seq = ?4 AND identity = ?1), \
                 ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
                 ON CONFLICT(identity, peer, fix_time) DO UPDATE SET \
                 via = excluded.via, message = excluded.message, \
                 received_at = excluded.received_at, telemetry = excluded.telemetry, \
                 source = excluded.source \
                 WHERE positions.source = 1 AND excluded.source = 0",
                params![
                    &identity[..],
                    &fix.peer[..],
                    &fix.via[..],
                    fix.message.map(seq_of),
                    l.last_update,
                    fix.received_at,
                    l.latitude_e6,
                    l.longitude_e6,
                    l.altitude_e2,
                    i64::from(l.speed_e2),
                    l.bearing_e2,
                    i64::from(l.accuracy_e2),
                    fix.telemetry,
                    source_code(fix.source),
                ],
            )?;
        }
    }
    Ok(())
}

/// `positions.source` of a fix's source (schema 14).
fn source_code(source: crate::telemetry::Source) -> i64 {
    match source {
        crate::telemetry::Source::Telemetry => 0,
        crate::telemetry::Source::Mesh => 1,
    }
}

/// The source `positions.source` names; an unknown code reads as a
/// message's, the column's default.
fn source_of(code: i64) -> crate::telemetry::Source {
    match code {
        1 => crate::telemetry::Source::Mesh,
        _ => crate::telemetry::Source::Telemetry,
    }
}

/// The key of the received message with this id, other than `except`.
fn received_key(
    conn: &Connection,
    identity: &[u8; 16],
    id: &[u8; 32],
    except: Option<i64>,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT seq FROM messages WHERE id = ?1 AND identity = ?2 AND direction = 0 \
         AND seq IS NOT ?3 LIMIT 1",
        params![&id[..], &identity[..], except],
        |row| row.get(0),
    )
    .optional()
}

fn row_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let id: Option<Vec<u8>> = row.get(1)?;
    let direction: i64 = row.get(2)?;
    let state: Option<i64> = row.get(3)?;
    let leg: Option<i64> = row.get(6)?;
    Ok(Message {
        outbound: direction == 1,
        body: row.get(10)?,
        id: id.and_then(|id| <[u8; 32]>::try_from(id.as_slice()).ok()),
        delivery: match state {
            Some(code) => delivery_of(code, row.get(12)?),
            None => None,
        },
        signature: signature_of(row.get(5)?),
        leg: None,
        unproven: row.get(4)?,
        at: wall_time(row.get(7)?, row.get(8)?),
        size: row.get::<_, i64>(9)?.max(0) as usize,
        ledger: Vec::new(),
        last_leg: leg.and_then(leg_of),
        activity: key_of(row.get(0)?),
        // Bytes that are not a fields map were not written by us: kept out
        // of the screen rather than misread.
        fields: row
            .get::<_, Option<Vec<u8>>>(11)?
            .and_then(|packed| unpack_fields(&packed))
            .unwrap_or_default(),
    })
}

/// `messages.verification`: 1 verified, as the boolean of schemas 1 and 2
/// wrote it, 0 unverified, 2 invalid.
pub(crate) fn signature_code(signature: Signature) -> i64 {
    match signature {
        Signature::Unverified => 0,
        Signature::Verified => 1,
        Signature::Invalid => 2,
    }
}

pub(crate) fn signature_of(code: i64) -> Signature {
    match code {
        0 => Signature::Unverified,
        1 => Signature::Verified,
        _ => Signature::Invalid,
    }
}

fn split(at: Option<WallTime>) -> (Option<i64>, Option<i64>) {
    match at {
        Some(at) => (Some(at.unix_secs), Some(i64::from(at.offset_secs))),
        None => (None, None),
    }
}

fn wall_time(secs: Option<i64>, offset: Option<i64>) -> Option<WallTime> {
    Some(WallTime {
        unix_secs: secs?,
        offset_secs: i32::try_from(offset.unwrap_or(0)).unwrap_or(0),
    })
}

/// The store's number for a delivery state. Stable: these are on disk.
pub(crate) fn state_code(state: DeliveryState) -> i64 {
    match state {
        DeliveryState::Queued => 0,
        DeliveryState::Stamping => 1,
        DeliveryState::InFlight => 2,
        DeliveryState::Sent => 3,
        DeliveryState::InMailbox => 4,
        DeliveryState::Delivered => 5,
        DeliveryState::Rejected => 6,
        DeliveryState::Failed => 7,
        DeliveryState::Read { .. } => 8,
        DeliveryState::Arrived => 9,
    }
}

/// `messages.read_at` for a state: the time of a read, NULL otherwise. A
/// read at 0 is one whose time nobody knows (an import's, 1506): NULL as
/// well, so no row ever keeps an invented epoch.
fn read_at(state: Option<DeliveryState>) -> Option<i64> {
    match state {
        Some(DeliveryState::Read { at }) if at > 0 => Some(i64::from(at)),
        _ => None,
    }
}

/// A row's state with its `read_at`: a read without one (a row another
/// writer left) is read at 0, the time unknown.
fn delivery_of(code: i64, read_at: Option<i64>) -> Option<DeliveryState> {
    match state_of(code)? {
        DeliveryState::Read { .. } => Some(DeliveryState::Read {
            at: read_at.and_then(|at| u32::try_from(at).ok()).unwrap_or(0),
        }),
        other => Some(other),
    }
}

pub(crate) fn state_of(code: i64) -> Option<DeliveryState> {
    Some(match code {
        0 => DeliveryState::Queued,
        1 => DeliveryState::Stamping,
        2 => DeliveryState::InFlight,
        3 => DeliveryState::Sent,
        4 => DeliveryState::InMailbox,
        5 => DeliveryState::Delivered,
        6 => DeliveryState::Rejected,
        7 => DeliveryState::Failed,
        8 => DeliveryState::Read { at: 0 },
        9 => DeliveryState::Arrived,
        _ => return None,
    })
}

fn leg_code(leg: Leg) -> i64 {
    match leg {
        Leg::Direct => 0,
        Leg::Propagated => 1,
    }
}

fn leg_of(code: i64) -> Option<Leg> {
    match code {
        0 => Some(Leg::Direct),
        1 => Some(Leg::Propagated),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: [u8; 16] = [0x01; 16];
    const ALICE: [u8; 16] = [0xa1; 16];
    const BOB: [u8; 16] = [0xb0; 16];

    fn at(secs: i64) -> Option<WallTime> {
        Some(WallTime {
            unix_secs: secs,
            offset_secs: 3600,
        })
    }

    /// A message id of its own for every key.
    fn id_of(key: u64) -> [u8; 32] {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&key.to_be_bytes());
        id
    }

    fn message(key: u64, outbound: bool, body: &str) -> Message {
        Message {
            outbound,
            body: body.to_string(),
            id: Some(id_of(key)),
            delivery: outbound.then_some(DeliveryState::Delivered),
            signature: Signature::Verified,
            leg: None,
            unproven: false,
            at: at(1_000 + key as i64),
            size: body.len(),
            ledger: vec![LedgerEntry {
                at: at(1_000 + key as i64),
                what: "queued".to_string(),
            }],
            last_leg: outbound.then_some(Leg::Direct),
            // The row at seq `key`, so the raw reads below can name it.
            activity: key_of(key as i64),
            fields: Vec::new(),
        }
    }

    fn put(store: &mut Store, peer: [u8; 16], message: Message) {
        store
            .write(&[StoreWrite::Message { peer, message }])
            .expect("write");
    }

    /// 1164: the data version moves for another connection's commit, an
    /// import's, and not for this connection's own append.
    #[test]
    fn the_data_version_moves_for_another_connections_commit_only() {
        let home = tempfile::tempdir().expect("home");
        let mut daemon = Store::open_in(home.path(), US).expect("open");
        let before = daemon.data_version().expect("version");
        daemon
            .archive_append(crate::home::KIND_RECEIVED, &[1; 32], b"own", 1, 1 << 20)
            .expect("own append");
        assert_eq!(daemon.data_version().expect("version"), before);
        let mut import = Store::open_in(home.path(), US).expect("a second connection");
        import
            .archive_append(crate::home::KIND_RECEIVED, &[2; 32], b"theirs", 2, 1 << 20)
            .expect("their append");
        let after = daemon.data_version().expect("version");
        assert_ne!(after, before);
        assert_eq!(daemon.data_version().expect("version"), after);
    }

    #[test]
    fn a_missing_file_starts_a_fresh_store() {
        let home = tempfile::tempdir().expect("home");
        let store = Store::open_in(home.path(), US).expect("a fresh store");
        assert!(store.restore().expect("restore").is_empty());
        assert_eq!(store.last_key().expect("last key"), KEY_OF_SEQ_ZERO);
        assert!(home.path().join(STORE_FILE).exists());
    }

    #[test]
    fn messages_their_ledger_and_the_pin_survive_a_reopen() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let mut sent = message(1, true, "are you at the field site?");
        put(&mut store, ALICE, sent.clone());
        put(&mut store, ALICE, message(2, false, "yes, from ten"));
        sent.ledger.push(LedgerEntry {
            at: at(1_010),
            what: "delivered (direct)".to_string(),
        });
        // 1230: a read with its time, in the row written over the first.
        sent.delivery = Some(DeliveryState::Read { at: 1_020 });
        put(&mut store, ALICE, sent.clone());
        store
            .write(&[StoreWrite::Conversation {
                peer: ALICE,
                pinned: true,
                unread: 1,
                flags: Flags {
                    muted_until: Some(i64::MAX),
                    archived: true,
                    verified: true,
                    receipts: true,
                },
            }])
            .expect("pin");
        drop(store);

        let store = Store::open_in(home.path(), US).expect("reopen");
        let restored = store.restore().expect("restore");
        assert_eq!(restored.len(), 1);
        let alice = &restored[0];
        assert_eq!(alice.peer, ALICE);
        assert!(alice.pinned);
        assert_eq!(alice.unread, 1);
        assert_eq!(alice.earlier, 0);
        // 1146: the mute, the archive and the verified mark survive.
        assert_eq!(alice.flags.muted_until, Some(i64::MAX));
        assert!(alice.flags.archived && alice.flags.verified);
        // 1230: the peer's receipt mark, and the read's time.
        assert!(alice.flags.receipts);
        assert_eq!(
            alice.messages[0].delivery,
            Some(DeliveryState::Read { at: 1_020 })
        );
        assert_eq!(
            alice.messages,
            vec![sent, message(2, false, "yes, from ten")]
        );
        assert_eq!(store.last_key().expect("last key"), key_of(2));
    }

    /// 1506: an imported message its source kept as read without a time
    /// is read at no known time. The store keeps `read_at` NULL (the column
    /// takes NULL since schema 9, no schema step), restores it as read at
    /// 0, and the Info sheet says "Read" without a time, never the epoch's.
    /// A read the store did know the time of keeps it when a second import
    /// lays the imported block out again.
    #[test]
    fn an_imported_read_message_has_no_read_time_and_never_the_epoch() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let row = |n: u8, delivery| ImportRow {
            id: [n; 32],
            outbound: true,
            timestamp: Some(1_700_000_000.0 + f64::from(n)),
            received_at: Some(1_700_000_000 + i64::from(n)),
            title: Vec::new(),
            content: format!("imported {n}"),
            fields: Vec::new(),
            delivery,
            signature: Signature::Unverified,
            origin_verdict: None,
            held: None,
        };
        let untimed = row(1, Some(DeliveryState::Read { at: 0 }));
        let timed = row(2, Some(DeliveryState::Read { at: 1_700_000_100 }));
        store
            .import_conversation(ALICE, "ratspeak", &[untimed.clone(), timed])
            .expect("import");
        let read_at = |store: &Store, id: [u8; 32]| -> Option<i64> {
            store
                .conn
                .query_row(
                    "SELECT read_at FROM messages WHERE id = ?1",
                    params![&id[..]],
                    |row| row.get(0),
                )
                .expect("the row")
        };
        assert_eq!(read_at(&store, [1; 32]), None, "never an invented time");
        assert_eq!(read_at(&store, [2; 32]), Some(1_700_000_100));
        // The archive entry carries the state alone and reads back the same.
        let entry = crate::import::archive::entry("ratspeak", &ALICE, &untimed);
        let (_, _, back) = crate::import::archive::read_entry(&entry).expect("entry");
        assert_eq!(back.delivery, Some(DeliveryState::Read { at: 0 }));
        // A second import lays the block out again; both keep what they had.
        store
            .import_conversation(ALICE, "ratspeak", &[row(3, None)])
            .expect("again");
        assert_eq!(read_at(&store, [1; 32]), None);
        assert_eq!(read_at(&store, [2; 32]), Some(1_700_000_100));
        let restored = store.restore().expect("restore");
        let alice = restored.iter().find(|c| c.peer == ALICE).expect("alice");
        let untimed = alice
            .messages
            .iter()
            .find(|m| m.id == Some([1; 32]))
            .expect("the untimed one");
        assert_eq!(untimed.delivery, Some(DeliveryState::Read { at: 0 }));

        // The words: "Read" alone, the timed one with its time.
        let mut model = crate::ui::UiModel::<crate::ui::tests::Field>::new(&[]);
        crate::ui::update(
            &mut model,
            crate::ui::UiEvent::Clock(WallTime {
                unix_secs: 1_700_100_000,
                offset_secs: 0,
            }),
        );
        model.restore(restored);
        let words = |id: [u8; 32]| -> Vec<(String, String)> {
            let key = model.conversations[0]
                .messages
                .iter()
                .find(|m| m.id == Some(id))
                .expect("it")
                .activity;
            crate::ui::info::message_info(&model, ALICE, key)
                .expect("info")
                .rows
                .iter()
                .map(|row| row.resolve(crate::ui::strings::Lang::En))
                .collect()
        };
        let state = |rows: &[(String, String)]| {
            rows.iter()
                .find(|(label, _)| label == "Status")
                .map(|(_, value)| value.clone())
        };
        let untimed_rows = words([1; 32]);
        assert_eq!(
            state(&untimed_rows).as_deref(),
            Some("Read"),
            "{untimed_rows:?}"
        );
        assert!(
            !untimed_rows
                .iter()
                .any(|(_, v)| v.contains("1970") || v.contains("00:00")),
            "no epoch: {untimed_rows:?}"
        );
        let timed_rows = words([2; 32]);
        assert_eq!(
            state(&timed_rows).as_deref(),
            Some("Read at 22:15"),
            "{timed_rows:?}"
        );
    }

    #[test]
    fn rows_are_scoped_to_our_address() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "for us"));
        drop(store);
        let other = Store::open_in(home.path(), [0x02; 16]).expect("open as another");
        assert!(other.restore().expect("restore").is_empty());
    }

    #[test]
    fn the_newest_window_loads_and_earlier_pages_follow() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let total = LOAD_WINDOW as u64 * 2 + 5;
        let writes: Vec<StoreWrite> = (1..=total)
            .map(|key| StoreWrite::Message {
                peer: BOB,
                message: message(key, key % 2 == 0, &format!("line {key}")),
            })
            .collect();
        store.write(&writes).expect("write");
        let restored = store.restore().expect("restore");
        let bob = &restored[0];
        assert_eq!(bob.messages.len(), LOAD_WINDOW);
        assert_eq!(bob.earlier, total as usize - LOAD_WINDOW);
        assert_eq!(
            bob.messages[0].activity,
            key_of((total - LOAD_WINDOW as u64 + 1) as i64)
        );
        assert_eq!(
            bob.messages.last().map(|m| m.activity),
            Some(key_of(total as i64))
        );

        let first = bob.messages[0].activity;
        let (page, earlier) = store.earlier(BOB, first, None).expect("earlier");
        assert_eq!(page.len(), LOAD_WINDOW);
        assert_eq!(earlier, 5);
        let (rest, earlier) = store
            .earlier(BOB, page[0].activity, None)
            .expect("the rest");
        assert_eq!(rest.len(), 5);
        assert_eq!(earlier, 0);
        assert_eq!(rest[0].activity, key_of(1));

        // Down to a key further back than one page.
        let (through, earlier) = store.earlier(BOB, first, Some(key_of(3))).expect("through");
        assert_eq!(through[0].activity, key_of(3));
        assert_eq!(earlier, 2);
    }

    #[test]
    fn a_received_message_is_one_row_however_often_it_arrives() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let direct = message(1, false, "the direct copy");
        let id = direct.id.expect("an id");
        assert!(!store.holds_received(&id).expect("ask"));
        put(&mut store, ALICE, direct.clone());
        assert!(store.holds_received(&id).expect("ask"));
        // Its own key again is an update, not a second copy.
        put(&mut store, ALICE, direct.clone());
        // The mailbox copy under a later key changes nothing.
        put(
            &mut store,
            ALICE,
            Message {
                activity: key_of(2),
                ..direct.clone()
            },
        );
        let restored = store.restore().expect("restore");
        assert_eq!(restored[0].messages, vec![direct]);
        assert_eq!(store.last_key().expect("last key"), key_of(1));
        // A sent message with the same id is not a received one.
        put(&mut store, BOB, message(3, true, "sent"));
        assert!(!store.holds_received(&id_of(3)).expect("ask"));
        // Nor is a row under another of our addresses.
        let other = Store::open_in(home.path(), [0x02; 16]).expect("as another");
        assert!(!other.holds_received(&id).expect("ask"));
    }

    /// 1643: the rooms a hub named when the home asked it ride its entry,
    /// a new entry only when they changed; the hourly refresh of the
    /// hearing keeps them; a hub the archive does not keep gets none. An
    /// entry without rooms reads as none, and the plain reader reads an
    /// entry with them as the hub.
    #[test]
    fn a_hubs_rooms_ride_its_entry_and_survive_the_refresh() {
        use crate::ui::hubs::HeardHub;
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let hub = |at: i64| HeardHub {
            hash: [1; 16],
            name: Some("Villa".into()),
            hops: Some(2),
            first: at,
            last: at,
            answered: None,
        };
        let rooms = |packed: &[u8]| {
            crate::home::wire::read_hub_entry_rooms(packed)
                .expect("a hub entry")
                .1
        };
        let newest = |store: &Store| {
            let page = store.archive_since(0, usize::MAX).expect("since");
            page.entries
                .iter()
                .rfind(|e| e.kind == crate::home::KIND_HUB)
                .map(|e| e.packed.clone())
                .expect("a hub entry")
        };
        assert_eq!(
            store
                .archive_hub_rooms(&[1; 16], &["#lab".into()], u64::MAX)
                .expect("rooms"),
            None,
            "a hub not kept gets no rooms"
        );
        let heard = store.archive_hub(&hub(1000), u64::MAX).expect("heard");
        assert_eq!(rooms(&newest(&store)), None);
        let named = vec!["#lab".to_string(), "#chat".to_string()];
        let with = store
            .archive_hub_rooms(&[1; 16], &named, u64::MAX)
            .expect("rooms");
        assert!(with > heard, "the rooms are news");
        assert_eq!(rooms(&newest(&store)), Some(named.clone()));
        assert_eq!(
            store
                .archive_hub_rooms(&[1; 16], &named, u64::MAX)
                .expect("again"),
            None,
            "the same rooms again are none"
        );
        let refreshed = store
            .archive_hub(&hub(1000 + 3600), u64::MAX)
            .expect("refresh");
        assert!(refreshed > with);
        assert_eq!(
            rooms(&newest(&store)),
            Some(named),
            "the refresh keeps them"
        );
        let plain = crate::home::wire::read_hub_entry(&newest(&store)).expect("plain");
        assert_eq!((plain.first, plain.last), (1000, 4600));
        assert_eq!(store.archived_hubs().expect("hubs").len(), 1);
        let none = store
            .archive_hub_rooms(&[1; 16], &[], u64::MAX)
            .expect("none");
        assert!(none.is_some(), "a hub that names none now");
        assert_eq!(rooms(&newest(&store)), Some(Vec::new()));
    }

    /// 1599: the home remembers the hubs it hears for its devices: one
    /// entry a hub, the newest, its first hearing kept; a hearing within
    /// the hour that says nothing new appends nothing; past the bound the
    /// hub heard longest ago goes. The control: a name entry is no hub.
    #[test]
    fn the_home_remembers_hubs_newest_once_and_bounded() {
        use crate::ui::hubs::{HeardHub, HEARD_BOUND};
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let hub = |hash: [u8; 16], name: Option<&str>, hops: u8, at: i64| HeardHub {
            hash,
            name: name.map(Into::into),
            hops: Some(hops),
            first: at,
            last: at,
            answered: None,
        };
        let first = store
            .archive_hub(&hub([1; 16], Some("Le Salon FR"), 9, 1000), u64::MAX)
            .expect("append");
        assert!(first.is_some(), "a new hub is news");
        assert_eq!(
            store
                .archive_hub(&hub([1; 16], Some("Le Salon FR"), 9, 1600), u64::MAX)
                .expect("ask"),
            None,
            "heard again within the hour, nothing new"
        );
        assert_eq!(
            store
                .archive_hub(&hub([1; 16], None, 9, 1700), u64::MAX)
                .expect("ask"),
            None,
            "a hearing without a name keeps the name"
        );
        let moved = store
            .archive_hub(&hub([1; 16], None, 7, 1800), u64::MAX)
            .expect("append");
        assert!(moved > first, "other hops are news");
        let later = store
            .archive_hub(&hub([1; 16], None, 7, 1800 + 3600), u64::MAX)
            .expect("append");
        assert!(later > moved, "an hour on, the last hearing is news");
        store
            .archive_name(ALICE, "Alice", 10, u64::MAX)
            .expect("name");
        let hubs = store.archived_hubs().expect("hubs");
        assert_eq!(hubs.len(), 1, "one entry a hub, and a name is no hub");
        assert_eq!(
            (
                hubs[0].name.as_deref(),
                hubs[0].hops,
                hubs[0].first,
                hubs[0].last
            ),
            (Some("Le Salon FR"), Some(7), 1000, 5400)
        );
        let page = store.archive_since(0, usize::MAX).expect("page");
        let kinds: Vec<u8> = page.entries.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [crate::home::KIND_HUB, crate::home::KIND_NAME]);
        // The bound: the hub heard longest ago goes.
        for i in 0..HEARD_BOUND {
            let mut hash = [2; 16];
            hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
            store
                .archive_hub(&hub(hash, None, 3, 10_000 + i as i64), u64::MAX)
                .expect("append");
        }
        let hubs = store.archived_hubs().expect("hubs");
        assert_eq!(hubs.len(), HEARD_BOUND);
        assert!(
            hubs.iter().all(|h| h.hash != [1; 16]),
            "Le Salon FR, heard longest ago, went"
        );
    }

    #[test]
    fn the_search_finds_only_what_lies_below_the_loaded_window() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "bring the Tripod"));
        put(&mut store, ALICE, message(2, true, "the tripod fits"));
        put(&mut store, BOB, message(3, false, "tripod in the van"));
        put(&mut store, ALICE, message(4, false, "no match here"));
        // Alice's screen loaded from key 2 on; Bob's holds all of his.
        let loaded: BTreeMap<[u8; 16], u64> = [(ALICE, key_of(2))].into_iter().collect();
        let hits = store.search("TRIPOD", &loaded).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, key_of(1));
        assert_eq!(hits[0].peer, ALICE);
        assert_eq!(hits[0].body, "bring the Tripod");
        // Two characters go past the trigram index, with the same answer.
        let short = store.search("tR", &loaded).expect("short search");
        assert_eq!(
            short.iter().map(|h| h.key).collect::<Vec<_>>(),
            vec![key_of(1)]
        );
        // LIKE's own characters are text.
        assert!(store.search("%", &loaded).expect("percent").is_empty());
        // A quote does not break the query.
        assert!(store.search("\"tri", &loaded).expect("quote").is_empty());
        // Nothing is below the window when every conversation is whole.
        assert!(store
            .search("tripod", &BTreeMap::new())
            .expect("whole")
            .is_empty());
    }

    #[test]
    fn deletes_remove_the_text_from_the_index_too() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "secret tripod"));
        put(&mut store, ALICE, message(2, false, "kept tripod"));
        put(&mut store, BOB, message(3, false, "bob's tripod"));
        store
            .write(&[StoreWrite::ForgetMessage { key: key_of(1) }])
            .expect("forget");
        let all: BTreeMap<[u8; 16], u64> =
            [(ALICE, u64::MAX), (BOB, u64::MAX)].into_iter().collect();
        let keys = |store: &Store| -> Vec<u64> {
            store
                .search("tripod", &all)
                .expect("search")
                .iter()
                .map(|h| h.key)
                .collect()
        };
        assert_eq!(keys(&store), vec![key_of(3), key_of(2)]);
        store
            .write(&[StoreWrite::ForgetConversation { peer: BOB }])
            .expect("forget bob");
        assert_eq!(keys(&store), vec![key_of(2)]);
        let restored = store.restore().expect("restore");
        assert_eq!(restored.len(), 1);
        let ledger_rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM ledger", [], |row| row.get(0))
            .expect("count");
        assert_eq!(ledger_rows, 1, "the ledger goes with its message");
    }

    #[test]
    fn names_are_kept_and_replaced() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        store.save_name(ALICE, "Alice").expect("save");
        store.save_name(ALICE, "Ally").expect("replace");
        store.save_name(BOB, "Bob").expect("save");
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        let mut names = store.names().expect("names");
        names.sort();
        assert_eq!(
            names,
            vec![(ALICE, "Ally".to_string()), (BOB, "Bob".to_string())]
        );
    }

    #[test]
    fn a_heard_name_is_archived_once_until_it_changes() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        store
            .archive_name(BOB, "Bob", 5, u64::MAX)
            .expect("append")
            .expect("Bob's name is news");
        let first = store
            .archive_name(ALICE, "Alice", 10, u64::MAX)
            .expect("append");
        assert!(first.is_some(), "a first name is news");
        assert_eq!(
            store
                .archive_name(ALICE, " Alice ", 20, u64::MAX)
                .expect("ask"),
            None,
            "the same name, as the clients will read it, appends nothing"
        );
        let blank = store
            .archive_name(ALICE, "\u{200b}", 25, u64::MAX)
            .expect("ask");
        assert_eq!(blank, None, "a name that sanitises to nothing says nothing");
        let renamed = store
            .archive_name(ALICE, "Ally", 30, u64::MAX)
            .expect("append");
        assert!(renamed > first, "a new name is a new entry");
        let back = store
            .archive_name(ALICE, "Alice", 40, u64::MAX)
            .expect("append");
        assert!(back > renamed, "and the old name again is one too");
        let page = store.archive_since(0, usize::MAX).expect("page");
        let names: Vec<_> = page
            .entries
            .iter()
            .map(|entry| {
                assert_eq!(entry.kind, crate::home::KIND_NAME);
                crate::home::wire::read_name_entry(&entry.packed).expect("a name entry")
            })
            .collect();
        // 1136: only the newest name entry of a peer is kept, and another
        // peer's stays.
        assert_eq!(
            names,
            [(BOB, "Bob".to_string()), (ALICE, "Alice".to_string())]
        );
        assert_eq!(
            page.entries[1].at, 40,
            "the entry's time is when it was heard"
        );
        assert_eq!(
            store.names().expect("names"),
            [(ALICE, "Alice".to_string()), (BOB, "Bob".to_string())]
        );
    }

    /// A store with some history in it, closed, and its length.
    fn written(home: &Path) -> u64 {
        let mut store = Store::open_in(home, US).expect("open");
        let writes: Vec<StoreWrite> = (1..=300)
            .map(|key| StoreWrite::Message {
                peer: ALICE,
                message: message(key, false, &"a long enough body ".repeat(8)),
            })
            .collect();
        store.write(&writes).expect("write");
        drop(store);
        std::fs::metadata(home.join(STORE_FILE))
            .expect("metadata")
            .len()
    }

    fn refused(home: &Path) -> String {
        match Store::open_in(home, US) {
            Err(error @ StoreError::Corrupt { .. }) => {
                let text = error.to_string();
                assert!(
                    text.contains(&home.join(STORE_FILE).display().to_string()),
                    "the refusal names the file: {text}"
                );
                text
            }
            Err(other) => panic!("refused, but not as damaged: {other}"),
            Ok(_) => panic!("a damaged store was opened"),
        }
    }

    /// The positive control first: the same file, whole, opens and holds
    /// its history; cut in half it is refused, never opened as empty.
    #[test]
    fn a_truncated_store_is_refused_and_left_as_it_is() {
        let home = tempfile::tempdir().expect("home");
        let length = written(home.path());
        let whole = Store::open_in(home.path(), US).expect("the whole file opens");
        assert_eq!(whole.restore().expect("restore")[0].earlier, 100);
        drop(whole);

        let path = home.path().join(STORE_FILE);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open for truncation");
        // The last page only, which nothing reads at open: SQLite compares
        // the file with its header's page count.
        file.set_len(length - 4096).expect("truncate a page");
        let text = refused(home.path());
        assert!(text.contains("malformed"), "{text}");
        // Half of it: SQLite finds the schema's pages gone.
        file.set_len(length / 2).expect("truncate to half");
        drop(file);
        refused(home.path());
        // Refusing wrote nothing over it.
        assert_eq!(
            std::fs::metadata(&path).expect("still there").len(),
            length / 2
        );
    }

    #[test]
    fn an_empty_or_foreign_file_is_refused() {
        let home = tempfile::tempdir().expect("home");
        let path = home.path().join(STORE_FILE);
        std::fs::write(&path, b"").expect("empty file");
        refused(home.path());
        std::fs::write(
            &path,
            b"this is a text file, not a database at all, honestly".repeat(100),
        )
        .expect("text file");
        refused(home.path());
        // Another program's SQLite file.
        std::fs::remove_file(&path).expect("remove");
        let foreign = Connection::open(&path).expect("foreign");
        foreign
            .execute_batch("CREATE TABLE notes(text TEXT); INSERT INTO notes VALUES ('x');")
            .expect("foreign schema");
        drop(foreign);
        let text = refused(home.path());
        assert!(text.contains("not a Loquax store"), "{text}");
    }

    #[test]
    fn a_store_from_a_later_loquax_is_refused() {
        let home = tempfile::tempdir().expect("home");
        written(home.path());
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        conn.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1))
            .expect("bump");
        drop(conn);
        assert!(matches!(
            Store::open_in(home.path(), US),
            Err(StoreError::Newer { .. })
        ));
    }

    /// A write is all or none: one that fails half way leaves the store as
    /// it was before it.
    #[test]
    fn a_failed_write_leaves_nothing_half_written() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, ALICE, message(1, false, "before"));
        // The second write's ledger line collides with one already there,
        // so the transaction fails after its first message went in.
        store
            .conn
            .execute(
                "INSERT INTO ledger(message, line, what) VALUES (1, 5, 'planted')",
                [],
            )
            .expect("plant");
        let mut bad = message(1, false, "before");
        bad.ledger = (0..6)
            .map(|i| LedgerEntry {
                at: None,
                what: format!("line {i}"),
            })
            .collect();
        let result = store.write(&[
            StoreWrite::Message {
                peer: BOB,
                message: message(2, false, "half"),
            },
            StoreWrite::Message {
                peer: ALICE,
                message: bad,
            },
        ]);
        assert!(result.is_err(), "the collision fails the write");
        let restored = store.restore().expect("restore");
        assert!(
            restored.iter().all(|c| c.peer != BOB),
            "the first half of the failed write is not in the store"
        );
    }

    // Telemetry (loquax-map.md, slice (a), "Stored where").

    fn fix(peer: [u8; 16], via: [u8; 16], message: Option<u64>, fix_time: i64) -> Fix {
        Fix {
            peer,
            via,
            message,
            received_at: Some(fix_time + 30),
            location: Location {
                latitude_e6: 43_773_100,
                longitude_e6: 11_256_000,
                altitude_e2: -150,
                speed_e2: 4_000_000_000,
                bearing_e2: 27_000,
                accuracy_e2: 65_535,
                last_update: fix_time,
            },
            telemetry: vec![0x81, 0x01, 0x05],
            source: crate::telemetry::Source::Telemetry,
        }
    }

    #[test]
    fn fields_are_kept_raw_and_come_back_with_their_message() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let mut report = message(1, false, "");
        report.fields = vec![(0x02, vec![0xc4, 0x01, 0x80]), (0x10, vec![0xc0])];
        put(&mut store, BOB, report.clone());
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(store.restore().expect("restore")[0].messages, vec![report]);
        let raw: Vec<u8> = store
            .conn
            .query_row("SELECT fields FROM messages WHERE seq = 1", [], |row| {
                row.get(0)
            })
            .expect("the column");
        assert_eq!(raw, vec![0x82, 0x02, 0xc4, 0x01, 0x80, 0x10, 0xc0]);
    }

    #[test]
    fn a_fix_arriving_twice_is_one_row_and_the_latest_comes_back() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, BOB, message(1, false, ""));
        put(&mut store, ALICE, message(2, false, ""));
        let direct = fix(BOB, BOB, Some(key_of(1)), 5_000);
        let relayed = fix(BOB, ALICE, Some(key_of(2)), 5_000);
        let older = fix(BOB, BOB, None, 4_000);
        store
            .write(&[
                StoreWrite::Position(direct.clone()),
                StoreWrite::Position(relayed),
                StoreWrite::Position(older),
            ])
            .expect("write");
        assert_eq!(
            store.track_len(BOB).expect("track"),
            2,
            "the repeat is one row"
        );
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(
            store.positions().expect("positions"),
            vec![direct.clone()],
            "integers as they were"
        );
        // 1146: the track, newest first, as many as asked.
        let track = store.track(BOB, 5).expect("track");
        assert_eq!(
            track.iter().map(Fix::fix_time).collect::<Vec<_>>(),
            [5_000, 4_000]
        );
        assert_eq!(store.track(BOB, 1).expect("track"), vec![direct]);
        assert!(store.track(ALICE, 5).expect("track").is_empty());
        // Scoped to our address like every table.
        let other = Store::open_in(home.path(), [0x02; 16]).expect("as another");
        assert!(other.positions().expect("positions").is_empty());
    }

    /// 1385: an announced fix is kept and comes back as announced; the
    /// same fix in a message takes its row and names the message; the
    /// reader gives a peer's window in fix-time order.
    #[test]
    fn announced_fixes_are_kept_and_a_message_takes_their_row() {
        use crate::telemetry::Source;
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, BOB, message(1, false, ""));
        let mesh = |t: i64| Fix {
            message: None,
            telemetry: Vec::new(),
            source: Source::Mesh,
            ..fix(BOB, BOB, None, t)
        };
        store
            .write(&[
                StoreWrite::Position(mesh(3_000)),
                StoreWrite::Position(mesh(5_000)),
                StoreWrite::Position(mesh(5_000)),
                StoreWrite::Position(mesh(4_000)),
            ])
            .expect("write");
        assert_eq!(
            store.track_len(BOB).expect("track"),
            3,
            "one row a fix time"
        );
        let told = fix(BOB, BOB, Some(key_of(1)), 5_000);
        store
            .write(&[StoreWrite::Position(told.clone())])
            .expect("write");
        // Announced again after the message: the message's row stays.
        store
            .write(&[StoreWrite::Position(mesh(5_000))])
            .expect("write");
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        let window = store
            .fixes_between(Some(BOB), 3_500, 6_000)
            .expect("window");
        assert_eq!(window, vec![mesh(4_000), told], "in fix-time order");
        let all = store.fixes_between(None, i64::MIN, i64::MAX).expect("all");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].source, Source::Mesh);
    }

    /// 1385's store numbers: a day at a 5 min cadence is 288 rows; a year
    /// of one peer is 105 120 rows, and the file it takes is printed for
    /// the report (`-- --nocapture`). A stored announced fix carries no
    /// Telemeter, so a row is the key, the integers and the column heads.
    #[test]
    fn a_year_of_one_peer_at_five_minutes() {
        use crate::telemetry::Source;
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let per_day = 86_400 / 300;
        assert_eq!(per_day, 288);
        let start = 1_700_000_000i64;
        let rows: Vec<StoreWrite> = (0..365 * per_day)
            .map(|i| {
                let t = start + i * 300;
                let mut f = fix(BOB, BOB, None, t);
                f.source = Source::Mesh;
                f.telemetry = Vec::new();
                f.location.latitude_e6 += (i % 977) as i32 * 13;
                f.location.longitude_e6 += (i % 1009) as i32 * 17;
                f.location.speed_e2 = (i % 300) as u32;
                f.location.bearing_e2 = (i % 36_000) as i32;
                f.location.accuracy_e2 = 800;
                StoreWrite::Position(f)
            })
            .collect();
        for chunk in rows.chunks(10_000) {
            store.write(chunk).expect("write");
        }
        assert_eq!(store.track_len(BOB).expect("track"), 105_120);
        store
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
            .expect("vacuum");
        let size = std::fs::metadata(home.path().join(STORE_FILE))
            .expect("file")
            .len();
        let empty = {
            let other = tempfile::tempdir().expect("other");
            let s = Store::open_in(other.path(), US).expect("open");
            s.conn.execute_batch("VACUUM;").expect("vacuum");
            std::fs::metadata(other.path().join(STORE_FILE))
                .expect("file")
                .len()
        };
        let per_row = (size - empty) as f64 / 105_120.0;
        std::println!(
            "TRACK-STORE rows=105120 file={size} empty={empty} per_row={per_row:.1} bytes"
        );
        assert!(size - empty < 16 * 1024 * 1024, "{size}");
    }

    /// 1201: the positions we shared, newest first, each with whom and
    /// when; a text of ours and a report we received are none of them.
    #[test]
    fn the_positions_we_shared_are_listed_newest_first_with_whom() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let place = |lat: i64, t: i64| Location::saturating(lat, 13_000_000, 0, 0, 0, 0, t);
        let mut first = message(1, true, "");
        first.fields = crate::ui::position_fields(place(52_000_000, 900));
        put(&mut store, BOB, first);
        put(&mut store, BOB, message(2, true, "just text"));
        let mut theirs = message(3, false, "");
        theirs.fields = crate::ui::position_fields(place(48_000_000, 950));
        put(&mut store, BOB, theirs);
        let mut second = message(4, true, "");
        second.fields = crate::ui::position_fields(place(53_000_000, 990));
        put(&mut store, ALICE, second);
        let shared = store.shared(10).expect("shared");
        let seen: Vec<([u8; 16], i32, Option<i64>)> = shared
            .iter()
            .map(|(fix, sent)| (fix.via, fix.location.latitude_e6, *sent))
            .collect();
        assert_eq!(
            seen,
            vec![
                (ALICE, 53_000_000, Some(1_004)),
                (BOB, 52_000_000, Some(1_001))
            ]
        );
        assert!(shared.iter().all(|(fix, _)| fix.peer == US));
        assert_eq!(store.shared(1).expect("one").len(), 1);
        let other = Store::open_in(home.path(), [0x02; 16]).expect("as another");
        assert!(other.shared(10).expect("shared").is_empty());
    }

    /// The second audit of 1757: a message of ours whose `lev` names BOB
    /// as a group copy's writer lists our position, not BOB's.
    #[test]
    fn a_position_we_shared_naming_a_writer_is_ours() {
        use leviculum_lxmf::lev::{GroupCopy, Lev};
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let place = Location::saturating(52_000_000, 13_000_000, 0, 0, 0, 0, 900);
        let mut ours = message(1, true, "");
        Lev {
            group_copy: Some(GroupCopy {
                sender: BOB,
                sent_ms: 1,
                epoch: 1,
            }),
            ..Lev::default()
        }
        .put_into(&mut ours.fields);
        ours.fields.extend(crate::ui::position_fields(place));
        put(&mut store, ALICE, ours);
        let shared = store.shared(10).expect("shared");
        let seen: Vec<([u8; 16], [u8; 16], i32)> = shared
            .iter()
            .map(|(fix, _)| (fix.peer, fix.via, fix.location.latitude_e6))
            .collect();
        assert_eq!(seen, [(US, ALICE, 52_000_000)]);
    }

    #[test]
    fn deleting_a_message_keeps_its_position_and_a_conversation_takes_them() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        put(&mut store, BOB, message(1, false, ""));
        store
            .write(&[StoreWrite::Position(fix(BOB, BOB, Some(1), 5_000))])
            .expect("write");
        store
            .write(&[StoreWrite::ForgetMessage { key: 1 }])
            .expect("forget the message");
        let left = store.positions().expect("positions");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].message, None, "the reference is cleared");
        store
            .write(&[StoreWrite::ForgetConversation { peer: BOB }])
            .expect("forget bob");
        assert!(store.positions().expect("positions").is_empty());
    }

    /// The schema 1 store of slice 6, as that loquax wrote it.
    fn v1_store(home: &Path) {
        let conn = Connection::open(home.join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = 1; COMMIT;"))
            .expect("schema 1");
        conn.execute_batch(
            "INSERT INTO conversations(identity, peer) VALUES (X'01010101010101010101010101010101', \
             X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1'); \
             INSERT INTO messages(seq, identity, conversation, direction, verification, \
             received_at, utc_offset, size, content) VALUES (7, \
             X'01010101010101010101010101010101', X'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1', 0, 1, \
             1000, 3600, 5, 'older');",
        )
        .expect("a message");
    }

    #[test]
    fn a_schema_1_store_migrates_in_place_and_keeps_its_history() {
        let home = tempfile::tempdir().expect("home");
        v1_store(home.path());
        let mut store = Store::open_in(home.path(), US).expect("schema 1 opens");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            SCHEMA_VERSION
        );
        let restored = store.restore().expect("restore");
        assert_eq!(restored[0].messages[0].body, "older");
        assert!(restored[0].messages[0].fields.is_empty());
        put(&mut store, ALICE, message(8, false, ""));
        store
            .write(&[StoreWrite::Position(fix(ALICE, ALICE, Some(8), 9_000))])
            .expect("a position in the migrated store");
        drop(store);
        // Opened again it is a current store, not migrated twice.
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(store.positions().expect("positions").len(), 1);
    }

    /// 1305: a migration says where it came from, where it went, how many
    /// steps and how long; a current store says nothing; a migration that
    /// fails says it was refused.
    #[test]
    fn a_migration_says_one_line_and_a_failed_one_says_it_was_refused() {
        use leviculum_std::test_support::log_capture::{capture_logs, captured};
        let home = tempfile::tempdir().expect("home");
        v1_store(home.path());
        let (buf, guard) = capture_logs();
        drop(Store::open_in(home.path(), US).expect("schema 1 opens"));
        drop(Store::open_in(home.path(), US).expect("reopen"));
        drop(guard);
        let log = captured(&buf);
        let line = format!(
            "HOME store migrated from=1 to={SCHEMA_VERSION} steps={}",
            SCHEMA_VERSION - 1
        );
        assert_eq!(log.matches(&line).count(), 1, "one line, once: {log}");
        assert!(log.contains("took_ms="), "{log}");

        // A schema 1 store that already holds schema 2's table: the step
        // to 2 fails, and nothing moves.
        let broken = tempfile::tempdir().expect("home");
        v1_store(broken.path());
        Connection::open(broken.path().join(STORE_FILE))
            .expect("open")
            .execute_batch("CREATE TABLE positions(x INTEGER);")
            .expect("a table in the way");
        let (buf, guard) = capture_logs();
        assert!(Store::open_in(broken.path(), US).is_err());
        drop(guard);
        let log = captured(&buf);
        assert!(
            log.contains(&format!(
                "HOME store migration refused from=1 to={SCHEMA_VERSION}"
            )),
            "{log}"
        );
        assert!(!log.contains("HOME store migrated"), "{log}");
    }

    /// A store this loquax wrote, opened by the loquax before it: the check
    /// of that program, which knew schema 1 only, refuses it as newer. Run
    /// here as the same comparison against version 1.
    #[test]
    fn a_schema_2_store_is_newer_than_a_schema_1_reader() {
        let home = tempfile::tempdir().expect("home");
        drop(Store::open_in(home.path(), US).expect("fresh"));
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        assert!(
            version > 1,
            "store.rs's check refuses `version > SCHEMA_VERSION`"
        );
    }

    #[test]
    fn a_schema_2_store_without_its_positions_is_refused() {
        let home = tempfile::tempdir().expect("home");
        drop(Store::open_in(home.path(), US).expect("fresh"));
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        conn.execute_batch("DROP TABLE positions").expect("drop");
        drop(conn);
        let text = refused(home.path());
        assert!(text.contains("table positions is missing"), "{text}");
    }

    /// A schema 4 store, as the home node of slice 1 wrote it, migrates to
    /// the current schema in place: its archive, devices and tokens stay, and
    /// the outbox starts empty.
    #[test]
    fn a_schema_4_store_migrates_with_an_empty_outbox() {
        let home = tempfile::tempdir().expect("home");
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} PRAGMA user_version = 4; COMMIT;"
        ))
        .expect("schema 4");
        drop(conn);
        {
            // Written the way slice 1 wrote them: no outbox table exists yet.
            let conn = Connection::open(home.path().join(STORE_FILE)).expect("reopen");
            conn.execute(
                "INSERT INTO archive(identity, kind, id, at, packed) VALUES (?1, 0, ?2, 1, ?3)",
                params![&US[..], &[7u8; 32][..], &b"old"[..]],
            )
            .expect("archive row");
            conn.execute(
                "INSERT INTO home_tokens(identity, token, name, created_at) VALUES (?1, ?2, 'x', 5)",
                params![&US[..], &[8u8; 16][..]],
            )
            .expect("token row");
        }
        let mut store = Store::open_in(home.path(), US).expect("schema 4 opens");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            SCHEMA_VERSION
        );
        assert_eq!(
            store
                .archive_since(0, usize::MAX)
                .expect("page")
                .entries
                .len(),
            1
        );
        assert_eq!(store.open_tokens(6, 100).expect("tokens").len(), 1);
        assert_eq!(store.outbox_open().expect("outbox"), []);
    }

    /// A schema 5 store, as the home node of slice 2 wrote it before the
    /// import existed, migrates to the current schema in place: an outbox
    /// row still retried stays, and its messages read as our own, with no
    /// origin.
    #[test]
    fn a_schema_5_store_migrates_keeping_its_outbox() {
        let home = tempfile::tempdir().expect("home");
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} {SCHEMA_V5} \
             PRAGMA user_version = 5; COMMIT;"
        ))
        .expect("schema 5");
        conn.execute(
            "INSERT INTO home_outbox(identity, client_id, id, destination, packed, \
             created_at, state, propagated) VALUES (?1, ?2, ?3, ?4, ?5, 9, 0, 1)",
            params![
                &US[..],
                &[1u8; 32][..],
                &[2u8; 32][..],
                &ALICE[..],
                &b"packed"[..]
            ],
        )
        .expect("outbox row");
        drop(conn);
        let store = Store::open_in(home.path(), US).expect("schema 5 opens");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            SCHEMA_VERSION
        );
        assert_eq!(
            store.outbox_open().expect("outbox"),
            [Outgoing {
                id: [2u8; 32],
                destination: ALICE,
                packed: b"packed".to_vec(),
                created_at: 9,
                propagated: true,
            }]
        );
        let origins: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE origin IS NOT NULL \
                 OR origin_verdict IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .expect("origin columns exist");
        assert_eq!(origins, 0);
    }

    /// A position share (1273) survives a restart as it stood: before its
    /// first report and after one, replaced in place, gone when stopped or
    /// with its conversation, and a row that is not whole is refused.
    #[test]
    fn a_share_is_kept_until_it_stops() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let fresh = Share::start(Span::UntilStopped, 1_793_500_000);
        store
            .write(&[StoreWrite::Share {
                peer: BOB,
                share: Some(fresh),
            }])
            .expect("write");
        assert_eq!(store.shares().expect("shares"), [(BOB, fresh)]);
        let mut sent = Share::start(Span::Hours8, 1_793_500_000);
        sent.last = Some(Report {
            fix: Location::saturating(
                -33_868_800,
                151_209_300,
                -1_250,
                0,
                0,
                65_535,
                1_793_499_990,
            ),
            at: 1_793_500_060,
            moved: true,
        });
        store
            .write(&[
                StoreWrite::Share {
                    peer: BOB,
                    share: Some(sent),
                },
                StoreWrite::Share {
                    peer: ALICE,
                    share: Some(fresh),
                },
            ])
            .expect("replace");
        assert_eq!(
            store.shares().expect("shares"),
            [(ALICE, fresh), (BOB, sent)],
            "in place, every integer as it was"
        );
        store
            .write(&[StoreWrite::Share {
                peer: ALICE,
                share: None,
            }])
            .expect("stop");
        store
            .write(&[StoreWrite::ForgetConversation { peer: BOB }])
            .expect("forget");
        assert!(store.shares().expect("shares").is_empty());
        store
            .write(&[StoreWrite::Share {
                peer: BOB,
                share: Some(sent),
            }])
            .expect("again");
        store
            .conn
            .execute("UPDATE shares SET last_at = NULL", [])
            .expect("break it");
        assert!(matches!(store.shares(), Err(StoreError::Corrupt { .. })));
        store
            .conn
            .execute("UPDATE shares SET last_at = 1, span = 'forever'", [])
            .expect("break it");
        assert!(matches!(store.shares(), Err(StoreError::Corrupt { .. })));
        drop(store);
        let other = Store::open_in(home.path(), [0x02; 16]).expect("other");
        assert!(other.shares().expect("shares").is_empty());
    }

    /// A schema 9 store, as 1270's loquax wrote it, gains the shares
    /// table and nothing else.
    #[test]
    fn a_schema_9_store_migrates_with_no_share() {
        let home = tempfile::tempdir().expect("home");
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} {SCHEMA_V5} {SCHEMA_V6} \
             {SCHEMA_V7} {SCHEMA_V8} {SCHEMA_V9} PRAGMA user_version = 9; COMMIT;"
        ))
        .expect("schema 9");
        drop(conn);
        let mut store = Store::open_in(home.path(), US).expect("schema 9 opens");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            SCHEMA_VERSION
        );
        assert!(store.shares().expect("shares").is_empty());
        store
            .write(&[StoreWrite::Share {
                peer: BOB,
                share: Some(Share::start(Span::Hour1, 5)),
            }])
            .expect("a share in the migrated store");
        drop(store);
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        conn.execute_batch("DROP TABLE shares").expect("drop");
        drop(conn);
        let text = refused(home.path());
        assert!(text.contains("table shares is missing"), "{text}");
    }

    /// 1635: the petnames a device owes its home node come back after a
    /// restart as they were kept, a cleared one as `None`; a write
    /// replaces all of them, and another identity sees none. A store of
    /// schema 18 gains the table on open.
    #[test]
    fn owed_petnames_survive_a_restart_and_a_schema_18_store_gains_them() {
        let home = tempfile::tempdir().expect("home");
        let owed = BTreeMap::from([([0x5a; 16], None), ([0x5b; 16], Some("Mum".to_string()))]);
        {
            let mut store = Store::open_in(home.path(), US).expect("open");
            assert!(store.owed_petnames().expect("read").is_empty());
            store
                .write(&[StoreWrite::OwedPetnames(owed.clone())])
                .expect("kept");
        }
        let mut store = Store::open_in(home.path(), US).expect("again");
        assert_eq!(store.owed_petnames().expect("read"), owed);
        let fewer = BTreeMap::from([([0x5b; 16], Some("Mum".to_string()))]);
        store
            .write(&[StoreWrite::OwedPetnames(fewer.clone())])
            .expect("replaced");
        assert_eq!(store.owed_petnames().expect("read"), fewer);
        let other = Store::open_in(home.path(), BOB).expect("other");
        assert!(other.owed_petnames().expect("read").is_empty());
        drop((store, other));

        let old = tempfile::tempdir().expect("an older home");
        drop(Store::open_in(old.path(), US).expect("create"));
        let conn = Connection::open(old.path().join(STORE_FILE)).expect("raw");
        conn.execute_batch(
            "DROP TABLE owed_petnames; ALTER TABLE home_devices DROP COLUMN last_seen; \
             DROP TABLE owed_peer_states; DROP TABLE home_names; PRAGMA user_version = 18;",
        )
        .expect("made schema 18");
        drop(conn);
        let mut store = Store::open_in(old.path(), US).expect("migrated");
        store
            .write(&[StoreWrite::OwedPetnames(fewer.clone())])
            .expect("kept");
        assert_eq!(store.owed_petnames().expect("read"), fewer);
    }

    /// 1643: a schema 21 store gains `home_names` on open, empty, and its
    /// pages keep their stamps: the first name heard takes the next one.
    /// Control: a store of this schema missing the table is refused as
    /// damaged, not migrated over.
    #[test]
    fn a_schema_21_store_gains_the_name_rows() {
        let old = tempfile::tempdir().expect("an older home");
        let mut store = Store::open_in(old.path(), US).expect("create");
        let page = store
            .page_heard(&lnomad::discovery::DiscoveredNode {
                dest_hash: [0x7c; 16],
                name: Some("Relay".into()),
                first_seen: 1,
                last_seen: 1,
                hops: None,
            })
            .expect("page")
            .expect("stamped");
        drop(store);
        let conn = Connection::open(old.path().join(STORE_FILE)).expect("raw");
        conn.execute_batch("DROP TABLE home_names; PRAGMA user_version = 21;")
            .expect("made schema 21");
        drop(conn);
        let mut store = Store::open_in(old.path(), US).expect("migrated");
        assert_eq!(store.names_kept().expect("count"), 0);
        let stamp = store
            .name_heard(&crate::home::pages::NameRow {
                address: [0x3c; 16],
                name: "Kaja".into(),
                first_seen: 2,
                last_seen: 2,
                hops: Some(1),
            })
            .expect("name")
            .expect("stamped");
        assert_eq!(stamp, page + 1);
        drop(store);
        let conn = Connection::open(old.path().join(STORE_FILE)).expect("raw");
        conn.execute_batch("DROP TABLE home_names;")
            .expect("damaged");
        drop(conn);
        assert!(matches!(
            Store::open_in(old.path(), US),
            Err(StoreError::Corrupt { .. })
        ));
    }

    /// 1642: a schema 19 store gains `home_devices.last_seen`, NULL for
    /// the devices it held; a request moves it, at most once a minute.
    #[test]
    fn a_schema_19_store_gains_when_each_device_was_last_there() {
        let old = tempfile::tempdir().expect("an older home");
        let mut store = Store::open_in(old.path(), US).expect("create");
        store.issue_token(&[1; 16], "Fairphone", 10).expect("token");
        store
            .redeem_token(&[1; 16], &[0x77; 16], 20, 3600)
            .expect("admitted");
        drop(store);
        let conn = Connection::open(old.path().join(STORE_FILE)).expect("raw");
        conn.execute_batch(
            "ALTER TABLE home_devices DROP COLUMN last_seen; DROP TABLE owed_peer_states; \
             DROP TABLE home_names; PRAGMA user_version = 19;",
        )
        .expect("made schema 19");
        drop(conn);
        let store = Store::open_in(old.path(), US).expect("migrated");
        let devices = store.devices().expect("devices");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].last_seen, None, "not heard since the migration");
        store.device_seen(&[0x77; 16], 100).expect("seen");
        store
            .device_seen(&[0x77; 16], 130)
            .expect("inside the minute");
        assert_eq!(store.devices().expect("devices")[0].last_seen, Some(100));
        store.device_seen(&[0x77; 16], 160).expect("a minute on");
        assert_eq!(store.devices().expect("devices")[0].last_seen, Some(160));
        store.device_seen(&[0x55; 16], 200).expect("a stranger");
        assert_eq!(store.devices().expect("devices").len(), 1);
    }

    /// 1640: the peer states a device owes its home node come back after a
    /// restart as they were kept, an unmuted one as `None` and an always
    /// mute as `i64::MAX`; a write replaces all of them, and another
    /// identity sees none. A store of schema 20 gains the table on open.
    #[test]
    fn owed_peer_states_survive_a_restart_and_a_schema_20_store_gains_them() {
        use crate::home::wire::PeerState;
        let home = tempfile::tempdir().expect("home");
        let owed = BTreeMap::from([
            ([0x5a; 16], PeerState::default()),
            (
                [0x5b; 16],
                PeerState {
                    blocked: true,
                    pinned: true,
                    archived: true,
                    muted_until: Some(i64::MAX),
                },
            ),
        ]);
        {
            let mut store = Store::open_in(home.path(), US).expect("open");
            assert!(store.owed_peer_states().expect("read").is_empty());
            store
                .write(&[StoreWrite::OwedPeerStates(owed.clone())])
                .expect("kept");
        }
        let mut store = Store::open_in(home.path(), US).expect("again");
        assert_eq!(store.owed_peer_states().expect("read"), owed);
        let fewer = BTreeMap::from([(
            [0x5b; 16],
            PeerState {
                muted_until: Some(1_800_000_000),
                ..PeerState::default()
            },
        )]);
        store
            .write(&[StoreWrite::OwedPeerStates(fewer.clone())])
            .expect("replaced");
        assert_eq!(store.owed_peer_states().expect("read"), fewer);
        let other = Store::open_in(home.path(), BOB).expect("other");
        assert!(other.owed_peer_states().expect("read").is_empty());
        drop((store, other));

        let old = tempfile::tempdir().expect("an older home");
        drop(Store::open_in(old.path(), US).expect("create"));
        let conn = Connection::open(old.path().join(STORE_FILE)).expect("raw");
        conn.execute_batch(
            "DROP TABLE owed_peer_states; DROP TABLE home_names; PRAGMA user_version = 20;",
        )
        .expect("made schema 20");
        drop(conn);
        let mut store = Store::open_in(old.path(), US).expect("migrated");
        store
            .write(&[StoreWrite::OwedPeerStates(fewer.clone())])
            .expect("kept");
        assert_eq!(store.owed_peer_states().expect("read"), fewer);
    }

    /// 1409: what the propagation node is still to be asked about comes
    /// back after a restart as it was kept, a write replaces all of it, and
    /// another identity sees none.
    #[test]
    fn what_the_node_is_to_be_asked_survives_a_restart() {
        use crate::pn_status::{AskerState, Open};
        let home = tempfile::tempdir().expect("home");
        let state = AskerState {
            open: vec![
                Open {
                    transient_id: [0x81; 32],
                    message_id: [0x61; 32],
                    node: [0x9c; 16],
                    uploaded_at: 1_800_000_000,
                },
                Open {
                    transient_id: [0x82; 32],
                    message_id: [0x62; 32],
                    node: [0x9d; 16],
                    uploaded_at: 1_800_000_100,
                },
            ],
            answering: vec![([0x9c; 16], 1)],
        };
        {
            let mut store = Store::open_in(home.path(), US).expect("open");
            assert_eq!(store.pn_asks().expect("read"), AskerState::default());
            store.save_pn_asks(&state).expect("kept");
        }
        let mut store = Store::open_in(home.path(), US).expect("again");
        assert_eq!(store.pn_asks().expect("read"), state);
        let fewer = AskerState {
            open: state.open[1..].to_vec(),
            answering: Vec::new(),
        };
        store.save_pn_asks(&fewer).expect("replaced");
        assert_eq!(store.pn_asks().expect("read"), fewer);
        let other = Store::open_in(home.path(), BOB).expect("other");
        assert_eq!(other.pn_asks().expect("read"), AskerState::default());
    }

    /// 1307 G2, item 2: a group the model wrote comes back with every
    /// field, and with no conversation row of its own (only its state
    /// came) it is listed still; written again it is replaced; it goes
    /// with its conversation, and another identity sees none.
    #[test]
    fn a_group_the_device_knows_comes_back_without_its_messages() {
        use crate::ui::groups::{GroupInfo, GroupMember, Standing};
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let group = GroupInfo {
            address: [0x22; 16],
            name: "Familie".into(),
            admin_name: "Lew".into(),
            inviter: Some(BOB),
            ours: false,
            epoch: 3,
            members: vec![
                GroupMember {
                    address: BOB,
                    name: "Lew".into(),
                    admin: true,
                    joined: true,
                    left: false,
                },
                GroupMember {
                    address: US,
                    name: "Me".into(),
                    admin: false,
                    joined: true,
                    left: false,
                },
                GroupMember {
                    address: [0x55; 16],
                    name: "Anna".into(),
                    admin: false,
                    joined: false,
                    left: true,
                },
            ],
            standing: Standing::Joined,
        };
        store
            .write(&[StoreWrite::Group(GroupInfo {
                epoch: 2,
                ..group.clone()
            })])
            .expect("write");
        store
            .write(&[StoreWrite::Group(group.clone())])
            .expect("again");
        let restored = store.restore().expect("restore");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].peer, group.address);
        assert!(restored[0].messages.is_empty());
        assert_eq!(restored[0].group.as_ref(), Some(&group));
        let other = Store::open_in(home.path(), BOB).expect("other");
        assert!(other.restore().expect("restore").is_empty());
        drop(other);
        store
            .write(&[StoreWrite::ForgetConversation {
                peer: group.address,
            }])
            .expect("forget");
        assert!(store.restore().expect("restore").is_empty());
    }

    /// 1219: a draft is kept beside its conversation, comes back at start
    /// with its reply, is replaced in place, and goes when empty or with
    /// its conversation; a conversation holding nothing but a draft comes
    /// back too.
    #[test]
    fn a_draft_is_kept_beside_its_conversation() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("open");
        let draft = |text: &str, reply: Option<[u8; 32]>| Draft {
            text: text.to_string(),
            reply,
            attachments: Vec::new(),
        };
        store
            .write(&[StoreWrite::Draft {
                peer: BOB,
                draft: draft("on my", Some([7; 32])),
            }])
            .expect("write");
        store
            .write(&[StoreWrite::Draft {
                peer: BOB,
                draft: draft("on my way", Some([7; 32])),
            }])
            .expect("replace");
        let restored = store.restore().expect("restore");
        assert_eq!(restored.len(), 1, "a draft alone lists its conversation");
        assert_eq!(restored[0].peer, BOB);
        assert_eq!(restored[0].draft, draft("on my way", Some([7; 32])));
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM drafts", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 1, "one row a conversation");
        store
            .write(&[StoreWrite::Draft {
                peer: BOB,
                draft: Draft::default(),
            }])
            .expect("clear");
        assert_eq!(store.restore().expect("restore")[0].draft, Draft::default());
        store
            .write(&[StoreWrite::Draft {
                peer: BOB,
                draft: draft("again", None),
            }])
            .expect("write");
        store
            .write(&[StoreWrite::ForgetConversation { peer: BOB }])
            .expect("forget");
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM drafts", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 0, "the draft goes with its conversation");
        // Another identity's store does not see it.
        store
            .write(&[StoreWrite::Draft {
                peer: BOB,
                draft: draft("mine", None),
            }])
            .expect("write");
        drop(store);
        let other = Store::open_in(home.path(), [0x02; 16]).expect("other");
        assert!(other.restore().expect("restore").is_empty());
    }

    /// 1219: a schema 7 store migrates to 8 in place, its conversations as
    /// they were and no draft in them.
    #[test]
    fn a_schema_7_store_migrates_with_no_drafts() {
        let home = tempfile::tempdir().expect("home");
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} {SCHEMA_V5} \
             {SCHEMA_V6} {SCHEMA_V7} PRAGMA user_version = 7; COMMIT;"
        ))
        .expect("schema 7");
        conn.execute(
            "INSERT INTO conversations(identity, peer, pinned) VALUES (?1, ?2, 1)",
            params![&US[..], &BOB[..]],
        )
        .expect("a pinned conversation");
        drop(conn);
        let store = Store::open_in(home.path(), US).expect("schema 7 opens");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            SCHEMA_VERSION
        );
        let restored = store.restore().expect("restore");
        assert!(restored[0].pinned);
        assert_eq!(restored[0].draft, Draft::default());
    }

    /// A schema 8 store, the one before read receipts (1230), migrates in
    /// place: its delivered message stays delivered, and no peer is known
    /// to send receipts.
    #[test]
    fn a_schema_8_store_migrates_with_no_receipts() {
        let home = tempfile::tempdir().expect("home");
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} {SCHEMA_V5} \
             {SCHEMA_V6} {SCHEMA_V7} {SCHEMA_V8} PRAGMA user_version = 8; COMMIT;"
        ))
        .expect("schema 8");
        conn.execute(
            "INSERT INTO messages(seq, identity, conversation, id, direction, state, \
             verification, size, content) VALUES (1, ?1, ?2, ?3, 1, 5, 1, 2, 'hi')",
            params![&US[..], &BOB[..], &[7u8; 32][..]],
        )
        .expect("a delivered message");
        conn.execute(
            "INSERT INTO conversations(identity, peer) VALUES (?1, ?2)",
            params![&US[..], &BOB[..]],
        )
        .expect("its conversation");
        drop(conn);
        let store = Store::open_in(home.path(), US).expect("schema 8 opens");
        let restored = store.restore().expect("restore");
        assert_eq!(
            restored[0].messages[0].delivery,
            Some(DeliveryState::Delivered)
        );
        assert!(!restored[0].flags.receipts);
    }

    /// A schema 3 store, as the loquax before the home node wrote it,
    /// migrates to the current schema in place: its messages stay, and the
    /// archive starts empty.
    #[test]
    fn a_schema_3_store_migrates_with_an_empty_archive() {
        let home = tempfile::tempdir().expect("home");
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} PRAGMA user_version = 3; COMMIT;"
        ))
        .expect("schema 3");
        drop(conn);
        let mut store = Store::open_in(home.path(), US).expect("schema 3 opens");
        put(&mut store, ALICE, message(1, false, "kept"));
        drop(store);
        let mut store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            SCHEMA_VERSION
        );
        assert_eq!(store.restore().expect("restore")[0].messages.len(), 1);
        assert_eq!(
            store.archive_since(0, usize::MAX).expect("page").entries,
            []
        );
        assert_eq!(
            store
                .archive_append(0, &[1; 32], b"m", 1, u64::MAX)
                .expect("append"),
            Some(1)
        );
    }

    #[test]
    fn a_schema_4_store_without_its_archive_is_refused() {
        let home = tempfile::tempdir().expect("home");
        drop(Store::open_in(home.path(), US).expect("fresh"));
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("open");
        conn.execute_batch("DROP TABLE archive").expect("drop");
        drop(conn);
        let text = refused(home.path());
        assert!(text.contains("table archive is missing"), "{text}");
    }

    /// A schema 2 store, as the loquax before leviculum 1079 wrote it,
    /// migrates in place to this program's schema: its unverified message
    /// keeps its row and has no bytes to be checked with.
    #[test]
    fn a_schema_2_store_migrates_and_its_old_unverified_messages_stay() {
        let home = tempfile::tempdir().expect("home");
        let conn = Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} PRAGMA user_version = 2; COMMIT;"
        ))
        .expect("schema 2");
        drop(conn);
        let mut store = Store::open_in(home.path(), US).expect("schema 2 opens");
        let mut old = message(1, false, "older");
        old.signature = Signature::Unverified;
        put(&mut store, ALICE, old);
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert_eq!(
            store
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .expect("version"),
            SCHEMA_VERSION
        );
        assert!(store.unverified().expect("unverified").is_empty());
        assert_eq!(
            store.restore().expect("restore")[0].messages[0].signature,
            Signature::Unverified
        );
    }

    /// The bytes of an unverified message are kept until its verdict, which
    /// its row then says across a reopen, invalid as loudly as verified; the
    /// bytes of a deleted message are not handed back.
    #[test]
    fn an_unverified_message_keeps_its_bytes_until_its_verdict() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("fresh");
        for key in [1, 2, 3] {
            let mut received = message(key, false, "who?");
            received.signature = Signature::Unverified;
            put(&mut store, ALICE, received);
            store
                .keep_unverified(&id_of(key), &ALICE, &[key as u8; 120])
                .expect("keep");
        }
        store
            .keep_unverified(&id_of(1), &ALICE, &[9; 120])
            .expect("kept once");
        store
            .write(&[StoreWrite::ForgetMessage { key: key_of(3) }])
            .expect("forget 3");
        let mut held = store.unverified().expect("unverified");
        held.sort();
        assert_eq!(
            held,
            vec![
                (id_of(1), ALICE, vec![1; 120]),
                (id_of(2), ALICE, vec![2; 120])
            ]
        );
        store.settle_verdict(&id_of(1), true).expect("valid");
        store.settle_verdict(&id_of(2), false).expect("invalid");
        drop(store);
        let store = Store::open_in(home.path(), US).expect("reopen");
        assert!(store.unverified().expect("unverified").is_empty());
        let signatures: Vec<Signature> = store.restore().expect("restore")[0]
            .messages
            .iter()
            .map(|m| m.signature)
            .collect();
        assert_eq!(signatures, [Signature::Verified, Signature::Invalid]);
    }
}
