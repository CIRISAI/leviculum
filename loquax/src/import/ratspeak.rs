//! Ratspeak's message database, read once from a copy the user names
//! (leviculum 1150, Lew 2026-10-30): the history of one Ratspeak version,
//! lossy and marked as imported, not kept in step with Ratspeak's releases.
//!
//! Written from the schema of Ratspeak at commit 7da1fe3
//! (`crates/ratspeak-db/src/db.rs`, AGPL, read, not copied), whose
//! `SCHEMA_VERSION` is 45 (`db.rs:11`), and of 39a6323, the last commit at
//! 33 (`db.rs:11`, tables `db.rs:107-255`; leviculum 1158, the schema of
//! Lew's own database). Any other version is refused with the numbers read
//! and the one found: Ratspeak is an alpha whose schema moves every few
//! days, and a reader that guesses at a schema it was not written for
//! imports wrong history quietly.
//!
//! Schema 33 holds what this reader takes in the same columns as 45, with
//! the same meanings (unix seconds, the same state words, `db.rs:139-162`
//! and `ratspeak-runtime/src/lxmf.rs:1697-1712` at 39a6323); it lacks
//! `messages.audio_mode` (no voice notes yet) and `lxmf_user_deleted`,
//! which [`Db::history`] then reads as empty.
//!
//! ```text
//! schema_version(version)                                     db.rs:237
//! identities(hash, lxmf_hash, display_name, propagation_node) db.rs:241-254
//! contacts(dest_hash, identity_id, display_name, trust)       db.rs:256-266
//! messages(id, source, destination, content, title,           db.rs:268-294
//!          timestamp, state, direction, identity_id,
//!          attachment_name, image_name, reply_to_id,
//!          reply_to_preview, audio_mode (45 only), ...)
//! blocked_contacts(dest_hash, identity_id)                    db.rs:314-320
//! reactions(message_id, sender, emoji, timestamp,             db.rs:376-384
//!           identity_id)
//! lxmf_user_deleted(identity_id, message_id: blob32)          admission.rs:38-41
//! ```
//!
//! Every row is scoped by `identity_id`, the hex identity hash of one of
//! Ratspeak's profiles (`ratspeak-runtime/src/lxmf.rs:2162`); the profile's
//! delivery address is `identities.lxmf_hash`. A sent message has that
//! address as its `source`, a received one as its `destination`
//! (`lxmf.rs:2887-2904`, `ratspeak-runtime/src/lib.rs:5547-5562`).
//!
//! # Time
//!
//! `messages.timestamp` is unix seconds, not a wall clock: a sent message
//! carries the LXMF timestamp it was signed with (`lxmf.rs:2868-2873`), a
//! received one the moment Ratspeak stored it, `SystemTime::now()` since
//! the epoch (`lib.rs:609-614, 5516`), and either is nudged one millisecond
//! past the conversation's newest when it would not sort after it
//! (`db.rs:3283-3310`). The database records no zone and needs none: the
//! values are UTC already, and the import takes them as they are, on any
//! machine, in any zone, across any change of daylight saving time.
//!
//! # What is lost
//!
//! Ratspeak keeps no packed message, so there is no signature to check: every
//! row is "imported, no signature". No fields column either, so games and
//! telemetry are gone, and of attachments only the first one's name was
//! ever kept, which is counted and not imported. A received message has no
//! sender's clock, only Ratspeak's receive time. Channels are Reticulum
//! Relay Chat, not LXMF, and are not read.
//!
//! # Replies and reactions
//!
//! A reply keeps its target's id and a preview in `reply_to_id` and
//! `reply_to_preview`; where the target is imported too, the row carries
//! `FIELD_REPLY_TO` and the preview as `FIELD_REPLY_QUOTE`, the shape
//! [`leviculum_lxmf::reply`] reads. Where it is not (deleted there, or no
//! LXMF id) the reply comes as text alone and is counted.
//!
//! A reaction is no message in Ratspeak, a row of `reactions` naming the
//! target's id and the sender's delivery address (`lib.rs:2457-2486`,
//! `lxmf.rs:2297-2321` at 39a6323), and a removed one is deleted there. In
//! Loquax a reaction is a message carrying `FIELD_REACTION`, so each row
//! becomes one with an id of its own, the SHA-256 of what names it in
//! Ratspeak ([`reaction_id`]): the same row is the same message on every
//! run. Ratspeak's time for it is when it stored it, which is what it
//! shows. One whose target is not imported, or whose sender is neither side
//! of the target's conversation, is counted and left.

use std::path::{Path, PathBuf};

use leviculum_lxmf::constants::FIELD_REACTION;
use leviculum_lxmf::reaction::Reaction;
use leviculum_lxmf::reply::Reply;
use leviculum_ui::delivery::DeliveryState;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use super::ImportError;
use crate::address::{parse as parse_address, parse_id, to_hex};
use crate::names::{sanitise, Contact};
use crate::store::ImportRow;
use crate::ui::Signature;

/// The schemas this reader was written for (`db.rs:11` at 39a6323 and at
/// 7da1fe3).
pub const SCHEMAS: [i64; 2] = [33, 45];

/// The first bytes of every SQLite database file.
pub const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// A Ratspeak database, open read-only.
pub struct Db {
    path: PathBuf,
    conn: Connection,
    schema: i64,
}

/// One of Ratspeak's profiles, an identity with its own history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The identity hash, which scopes every row (`identity_id`).
    pub hash: String,
    /// The delivery address, when the row names a readable one.
    pub address: Option<[u8; 16]>,
    pub display_name: Option<String>,
    pub propagation_node: Option<[u8; 16]>,
    /// The messages Ratspeak holds for it.
    pub messages: usize,
}

/// One conversation of a profile, read whole.
#[derive(Debug, Default)]
pub struct ConversationRead {
    pub peer: [u8; 16],
    /// In Ratspeak's order, which is time order.
    pub rows: Vec<ImportRow>,
    /// Rows with an attachment, an image or a voice note, none imported.
    pub attachments: usize,
}

/// One conversation while it is read: each row with the time it sorts by.
#[derive(Default)]
struct Gathering {
    rows: Vec<(f64, ImportRow)>,
    attachments: usize,
}

/// A profile's history.
#[derive(Debug, Default)]
pub struct History {
    pub conversations: Vec<ConversationRead>,
    /// Rows Ratspeak's deletion record names: the user deleted them there.
    pub deleted: usize,
    /// Rows that are no message this reader can place, with the reason.
    pub unreadable: Vec<(String, String)>,
    /// Replies linked to their imported target.
    pub replies_linked: usize,
    /// Replies whose target is not imported: their text comes alone.
    pub replies_unlinked: usize,
    /// Reactions imported, as messages of their own.
    pub reactions: usize,
    /// Reactions left: the target is not imported, or the sender is
    /// neither side of its conversation, or nothing of the emoji is left.
    pub reactions_skipped: usize,
}

/// Whether `path` begins like an SQLite database.
pub fn looks_like(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok()
        && &head == SQLITE_MAGIC
}

impl Db {
    /// Open the database at `path` read-only and check that it is one of
    /// Ratspeak's [`SCHEMAS`]. Nothing is ever written to it.
    pub fn open(path: &Path) -> Result<Self, ImportError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn =
            Connection::open_with_flags(path, flags).map_err(|e| ImportError::new(path, e))?;
        let mut db = Self {
            path: path.to_path_buf(),
            conn,
            schema: 0,
        };
        if !db.has_table("schema_version")? || !db.has_table("messages")? {
            return Err(ImportError::new(
                path,
                "an SQLite database, but not Ratspeak's",
            ));
        }
        let version: Option<i64> = db
            .conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| db.failed(e))?;
        let known = SCHEMAS.map(|s| s.to_string()).join(" and ");
        match version {
            Some(found) if SCHEMAS.contains(&found) => {
                db.schema = found;
                Ok(db)
            }
            Some(found) => Err(ImportError::new(
                path,
                format!(
                    "written for Ratspeak schemas {known}, this db is {found}; \
                     nothing imported"
                ),
            )),
            None => Err(ImportError::new(
                path,
                format!("written for Ratspeak schemas {known}, this db records none"),
            )),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The schema version the database records, one of [`SCHEMAS`].
    pub fn schema(&self) -> i64 {
        self.schema
    }

    fn failed(&self, error: impl std::fmt::Display) -> ImportError {
        ImportError::new(&self.path, error)
    }

    fn has_table(&self, name: &str) -> Result<bool, ImportError> {
        self.conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![name],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
            .map_err(|e| self.failed(e))
    }

    /// Every profile, in the order of its identity hash.
    pub fn profiles(&self) -> Result<Vec<Profile>, ImportError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT i.hash, i.lxmf_hash, i.display_name, i.propagation_node, \
                 (SELECT COUNT(*) FROM messages m WHERE m.identity_id = i.hash) \
                 FROM identities i ORDER BY i.hash",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        Ok(rows
            .into_iter()
            .map(|(hash, address, name, pn, messages)| Profile {
                hash,
                address: address.as_deref().and_then(|a| parse_address(a).ok()),
                display_name: name.as_deref().and_then(sanitise),
                propagation_node: pn.as_deref().and_then(|a| parse_address(a).ok()),
                messages: messages.max(0) as usize,
            })
            .collect())
    }

    /// The profile's contacts: Ratspeak's name for each (user-chosen, or the
    /// announced one where the user gave none, `db.rs:2495-2522`) as the
    /// petname, and its blocks.
    pub fn contacts(&self, profile: &Profile) -> Result<Vec<Contact>, ImportError> {
        let mut out: Vec<Contact> = Vec::new();
        let mut named = self
            .conn
            .prepare(
                "SELECT dest_hash, display_name FROM contacts WHERE identity_id = ?1 \
                 ORDER BY dest_hash",
            )
            .map_err(|e| self.failed(e))?;
        let rows = named
            .query_map(params![profile.hash], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        for (peer, name) in rows {
            let Ok(peer) = parse_address(&peer) else {
                continue;
            };
            out.push(Contact {
                peer,
                petname: name.as_deref().and_then(sanitise),
                blocked: false,
            });
        }
        if self.has_table("blocked_contacts")? {
            let mut blocked = self
                .conn
                .prepare("SELECT dest_hash FROM blocked_contacts WHERE identity_id = ?1")
                .map_err(|e| self.failed(e))?;
            let rows = blocked
                .query_map(params![profile.hash], |row| row.get::<_, String>(0))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                .map_err(|e| self.failed(e))?;
            for peer in rows.iter().filter_map(|p| parse_address(p).ok()) {
                match out.iter_mut().find(|c| c.peer == peer) {
                    Some(contact) => contact.blocked = true,
                    None => out.push(Contact {
                        peer,
                        petname: None,
                        blocked: true,
                    }),
                }
            }
        }
        Ok(out)
    }

    /// The profile's messages, one conversation a peer, each in Ratspeak's
    /// order. A row the user deleted in Ratspeak stays deleted.
    pub fn history(&self, profile: &Profile, own: [u8; 16]) -> Result<History, ImportError> {
        let mut deleted_ids = std::collections::BTreeSet::new();
        if self.has_table("lxmf_user_deleted")? {
            let mut statement = self
                .conn
                .prepare("SELECT message_id FROM lxmf_user_deleted WHERE identity_id = ?1")
                .map_err(|e| self.failed(e))?;
            let rows = statement
                .query_map(params![profile.hash], |row| row.get::<_, Vec<u8>>(0))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
                .map_err(|e| self.failed(e))?;
            deleted_ids.extend(
                rows.into_iter()
                    .filter_map(|id| <[u8; 32]>::try_from(id).ok()),
            );
        }
        // Schema 33 had no voice notes, and no column for them.
        let audio = match self.schema {
            33 => "NULL",
            _ => "audio_mode",
        };
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT id, source, destination, content, title, timestamp, state, direction, \
                 attachment_name, image_name, {audio}, reply_to_id, reply_to_preview \
                 FROM messages WHERE identity_id = ?1 ORDER BY timestamp, rowid"
            ))
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![profile.hash], |row| {
                Ok(Raw {
                    id: row.get(0)?,
                    source: row.get(1)?,
                    destination: row.get(2)?,
                    content: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    title: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    timestamp: row.get(5)?,
                    state: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                    direction: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    attached: !row
                        .get::<_, Option<String>>(8)?
                        .unwrap_or_default()
                        .is_empty()
                        || !row
                            .get::<_, Option<String>>(9)?
                            .unwrap_or_default()
                            .is_empty()
                        || row.get::<_, Option<i64>>(10)?.is_some(),
                    reply_to: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
                    reply_preview: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
                })
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        let mut history = History::default();
        let mut gathered: Vec<([u8; 16], Gathering)> = Vec::new();
        // Each imported message's conversation, by its id.
        let mut placed: std::collections::BTreeMap<[u8; 32], [u8; 16]> = Default::default();
        let mut replies: Vec<([u8; 16], usize, String, String)> = Vec::new();
        for raw in rows {
            let id_text = raw.id.clone();
            let (at, reply_to, preview) = (
                raw.timestamp,
                raw.reply_to.clone(),
                raw.reply_preview.clone(),
            );
            match raw.row(own) {
                Err(reason) => history.unreadable.push((id_text, reason.to_string())),
                Ok((_, row, _)) if deleted_ids.contains(&row.id) => history.deleted += 1,
                Ok((peer, row, attached)) => {
                    let conversation = gathering(&mut gathered, peer);
                    conversation.attachments += usize::from(attached);
                    placed.insert(row.id, peer);
                    if !reply_to.is_empty() {
                        replies.push((peer, conversation.rows.len(), reply_to, preview));
                    }
                    conversation.rows.push((at, row));
                }
            }
        }
        for (peer, index, reply_to, preview) in replies {
            let target = parse_id(&reply_to).filter(|to| placed.contains_key(to));
            let Some(to) = target else {
                history.replies_unlinked += 1;
                continue;
            };
            let reply = Reply {
                to,
                quote: Some(preview).filter(|q| !q.is_empty()),
            };
            if let Some((_, row)) = gathering(&mut gathered, peer).rows.get_mut(index) {
                row.fields.extend(reply.fields());
                history.replies_linked += 1;
            }
        }
        for reaction in self.reactions(profile)? {
            match reaction.row(&profile.hash, own, &placed) {
                Some((peer, at, row)) => {
                    gathering(&mut gathered, peer).rows.push((at, row));
                    history.reactions += 1;
                }
                None => history.reactions_skipped += 1,
            }
        }
        history.conversations = gathered
            .into_iter()
            .map(|(peer, mut gathering)| {
                gathering.rows.sort_by(|(a, _), (b, _)| a.total_cmp(b));
                ConversationRead {
                    peer,
                    rows: gathering.rows.into_iter().map(|(_, row)| row).collect(),
                    attachments: gathering.attachments,
                }
            })
            .collect();
        Ok(history)
    }

    /// The profile's reactions, in the order Ratspeak stored them.
    fn reactions(&self, profile: &Profile) -> Result<Vec<RawReaction>, ImportError> {
        if !self.has_table("reactions")? {
            return Ok(Vec::new());
        }
        let mut statement = self
            .conn
            .prepare(
                "SELECT message_id, sender, emoji, timestamp FROM reactions \
                 WHERE identity_id = ?1 ORDER BY timestamp, id",
            )
            .map_err(|e| self.failed(e))?;
        statement
            .query_map(params![profile.hash], |row| {
                Ok(RawReaction {
                    target: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    sender: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    emoji: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    timestamp: row.get::<_, Option<f64>>(3)?.unwrap_or(f64::NAN),
                })
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))
    }
}

/// The conversation with `peer` in `gathered`, begun when it is new.
fn gathering(gathered: &mut Vec<([u8; 16], Gathering)>, peer: [u8; 16]) -> &mut Gathering {
    let index = match gathered.iter().position(|(p, _)| *p == peer) {
        Some(index) => index,
        None => {
            gathered.push((peer, Gathering::default()));
            gathered.len() - 1
        }
    };
    &mut gathered[index].1
}

/// One `reactions` row as Ratspeak wrote it.
struct RawReaction {
    target: String,
    sender: String,
    emoji: String,
    timestamp: f64,
}

impl RawReaction {
    /// The conversation, the time it sorts by and the row: a message
    /// carrying `FIELD_REACTION`. `None` when the target is not imported,
    /// the sender is neither side of its conversation, the time is none, or
    /// nothing of the emoji is left.
    fn row(
        &self,
        identity: &str,
        own: [u8; 16],
        placed: &std::collections::BTreeMap<[u8; 32], [u8; 16]>,
    ) -> Option<([u8; 16], f64, ImportRow)> {
        let to = parse_id(&self.target)?;
        let peer = *placed.get(&to)?;
        let sender = parse_address(&self.sender).ok()?;
        let outbound = match sender {
            s if s == own => true,
            s if s == peer => false,
            _ => return None,
        };
        let content = crate::ui::reactions::sanitise(&self.emoji)?;
        if !self.timestamp.is_finite() || self.timestamp < 0.0 {
            return None;
        }
        let field = Reaction {
            to,
            content: content.clone(),
        }
        .encode_field_value();
        let row = ImportRow {
            id: reaction_id(identity, &self.target, &self.sender, &self.emoji),
            outbound,
            timestamp: outbound.then_some(self.timestamp),
            received_at: Some(self.timestamp.floor() as i64),
            title: Vec::new(),
            content: String::new(),
            fields: vec![(FIELD_REACTION, field)],
            // Ratspeak records no delivery for a reaction it sent.
            delivery: outbound.then_some(DeliveryState::Sent),
            signature: Signature::Unverified,
            origin_verdict: None,
            held: None,
        };
        Some((peer, self.timestamp, row))
    }
}

/// The id an imported reaction is stored by: the SHA-256 of the columns
/// that make it unique in Ratspeak (`UNIQUE(message_id, sender, emoji,
/// identity_id)`, `db.rs:245-253` at 39a6323), each ended by a zero byte.
/// No LXMF message has it, so it never meets one.
pub fn reaction_id(identity: &str, target: &str, sender: &str, emoji: &str) -> [u8; 32] {
    let mut keyed = b"ratspeak reaction\0".to_vec();
    for part in [identity, target, sender, emoji] {
        keyed.extend_from_slice(part.as_bytes());
        keyed.push(0);
    }
    leviculum_core::crypto::sha256(&keyed)
}

/// One `messages` row as Ratspeak wrote it.
struct Raw {
    id: String,
    source: String,
    destination: String,
    content: String,
    title: String,
    timestamp: f64,
    state: String,
    direction: String,
    attached: bool,
    /// The id of the message this one replies to, or empty.
    reply_to: String,
    reply_preview: String,
}

impl Raw {
    /// The conversation's peer, the row as the store keeps it, and whether
    /// it had an attachment.
    fn row(self, own: [u8; 16]) -> Result<([u8; 16], ImportRow, bool), &'static str> {
        let id = parse_id(&self.id).ok_or("the id is not an LXMF message id")?;
        let outbound = match self.direction.as_str() {
            "outbound" => true,
            "inbound" => false,
            _ => return Err("neither sent nor received"),
        };
        let (from, to) = (
            parse_address(&self.source),
            parse_address(&self.destination),
        );
        let peer = match (outbound, from, to) {
            (true, Ok(from), Ok(to)) if from == own => to,
            (false, Ok(from), Ok(to)) if to == own => from,
            _ => return Err("its addresses do not name this identity"),
        };
        if !self.timestamp.is_finite() || self.timestamp < 0.0 {
            return Err("no time");
        }
        let row = ImportRow {
            id,
            outbound,
            // Our own clock was the sender's for a sent message; a received
            // one has only Ratspeak's receive time.
            timestamp: outbound.then_some(self.timestamp),
            received_at: Some(self.timestamp.floor() as i64),
            title: self.title.into_bytes(),
            content: self.content,
            fields: Vec::new(),
            delivery: outbound.then(|| delivery_of(&self.state)),
            signature: Signature::Unverified,
            origin_verdict: None,
            held: None,
        };
        Ok((peer, row, self.attached))
    }
}

/// Where a sent message stood in Ratspeak (`state`). A message Ratspeak was
/// still sending is not sent by Loquax, so it ends as failed: the reader
/// sees that it may never have left.
pub fn delivery_of(state: &str) -> DeliveryState {
    match state {
        // Ratspeak's delivered is a proof, its mailbox's or its device's:
        // one tick (1462). Read was on the device.
        "delivered" => DeliveryState::Arrived,
        "read" => DeliveryState::Delivered,
        "propagated" | "propagating" => DeliveryState::InMailbox,
        "sent" => DeliveryState::Sent,
        "rejected" => DeliveryState::Rejected,
        _ => DeliveryState::Failed,
    }
}

/// A profile as the dry run lists it.
pub fn profile_line(profile: &Profile, chosen: bool) -> String {
    let address = profile
        .address
        .map_or("no address".to_string(), |a| format!("<{}>", to_hex(&a)));
    let name = profile
        .display_name
        .as_deref()
        .map_or(String::new(), |n| format!(" \"{n}\""));
    let mark = match chosen {
        true => "imported",
        false => "not imported",
    };
    format!(
        "  identity {address}{name}  {} messages  {mark}",
        profile.messages
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_states_map_to_what_a_reader_can_trust() {
        assert_eq!(delivery_of("delivered"), DeliveryState::Arrived);
        assert_eq!(delivery_of("read"), DeliveryState::Delivered);
        assert_eq!(delivery_of("propagated"), DeliveryState::InMailbox);
        assert_eq!(delivery_of("sent"), DeliveryState::Sent);
        assert_eq!(delivery_of("sending"), DeliveryState::Failed);
        assert_eq!(delivery_of("cancelled"), DeliveryState::Failed);
    }
}
