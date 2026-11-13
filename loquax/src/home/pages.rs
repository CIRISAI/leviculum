//! What the home remembers of the NomadNet pages it hears, and the
//! bookmarks of its identity, for every device (1579, `loquax-mailbox.md`,
//! "Pages and bookmarks"), `no_std`, so a board can run the same rules.
//!
//! The home hears every `nomadnetwork.node` announce of its net and keeps a
//! row per node: lnomad's [`DiscoveredNode`] (newest name, first and last
//! seen, hops) and how many announces it heard. A device asks `/pages`
//! with the stamp of what it holds and gets the rows that changed after it,
//! so its first ask brings the whole list and later ones the deltas.
//!
//! A bookmark a device sets or clears goes to the home with `/bookmark`,
//! stamped with the device's clock, and comes back to every device of the
//! identity in the same `/pages` answers. The newer stamp wins
//! ([`bookmark_wins`]); a cleared bookmark is a tombstone, so a deletion
//! travels as a set does, and a device that was off when it happened still
//! learns it.
//!
//! The home also keeps the name of every LXMF delivery announce it hears
//! (1643, [`NameRow`]): address, name, first and last seen by the home's
//! clock, and hops. A device of an external mailbox hears no mesh of its
//! own, so this table is where its chats, contacts and new-conversation
//! list find the names of peers nobody wrote to yet. The archive's name
//! entries stay for the peers the user has a relation to, where every
//! entry is a wake; these rows wake nobody and ride in the same `/pages`
//! delta.
//!
//! ```text
//! /pages    [since: uint, (page: uint)]   -> [status, stamp: uint, more: bool, [row, ...]]
//!   row:    [0, dest: bin16, name: str, first: int, last: int, hops: uint, count: uint]
//!         | [1, url: str, title: str, at: int, deleted: bool]
//!         | [2, address: bin16, name: str, first: int, last: int, hops: uint]
//! /bookmark [url: str, title: str, at: int, deleted: bool] -> [status]
//! ```
//!
//! `stamp` is the home's change counter after the last row of the answer;
//! the device asks with it next time. `hops` is `u32::MAX` when unknown,
//! `name` empty when the announce carried none. A home from before 1579
//! has no handler for either path and stays silent; the device keeps the
//! list it heard itself, as a device without a home always does.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::msgpack;
use lnomad::discovery::DiscoveredNode;
use lnomad::{Bookmark, Bookmarks};

use super::wire::{self, Malformed, Status};

/// The most page rows the home keeps per identity. Past it the row heard
/// longest ago goes (`last_seen` oldest first). 4096 rows at about 60
/// bytes are a quarter megabyte in the store, and a busy mesh announces a
/// few hundred NomadNet nodes, so the cap is room to spare, not a loss.
pub const PAGES_CAP: usize = 4096;

/// The most bookmark rows the home keeps per identity, tombstones
/// included. Past it the oldest tombstone goes first, then the oldest
/// bookmark.
pub const BOOKMARKS_CAP: usize = 1024;

/// How long a node heard again goes before its row takes a new stamp with
/// the later `last_seen` (as [`super::HUB_REFRESH_SECS`]): a NomadNet node
/// announces every few minutes to every few hours, and a device needs its
/// last seen to the hour, not each announce. Its count rises with every
/// announce; that alone sends nothing.
pub const PAGE_REFRESH_SECS: i64 = 3600;

/// The most name rows the home keeps per identity (1643). Past it the row
/// heard longest ago goes (`last_seen` oldest first), never the one just
/// heard. 4096 rows at about 50 bytes are 200 KiB in the store; a busy
/// mesh of 2026 announces a few hundred LXMF addresses.
pub const NAMES_CAP: usize = 4096;

/// How long a peer heard again with the same name and hops goes before its
/// name row takes a new stamp with the later `last_seen` (1643), as
/// [`PAGE_REFRESH_SECS`]: an LXMF client announces every few minutes, and
/// a device needs its last seen to the hour, not each announce.
pub const NAME_REFRESH_SECS: i64 = 3600;

/// The most bytes one `/pages` answer takes when the device asked no
/// bound: a full list of 2000 rows is about four answers, each one
/// resource over TCP or BLE, and the home never builds the whole list in
/// one buffer.
pub const PAGES_ANSWER_MAX: usize = 32 * 1024;

/// The hops written for a node whose hops are unknown.
const HOPS_UNKNOWN: u64 = u32::MAX as u64;

const ROW_PAGE: u64 = 0;
const ROW_BOOKMARK: u64 = 1;
const ROW_NAME: u64 = 2;

/// One node the home heard announcing its pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRow {
    pub node: DiscoveredNode,
    /// How many of its announces the home heard.
    pub count: u64,
}

/// One bookmark of the identity, or its tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkRow {
    /// lnomad's page URL, `<dest_hex>:<path>`: the bookmark's key.
    pub url: String,
    /// The label it was saved under.
    pub title: String,
    /// The writing device's clock, unix milliseconds: the newer wins.
    pub at: i64,
    /// A cleared bookmark: kept as a tombstone, so the clearing travels.
    pub deleted: bool,
}

/// The name of one LXMF delivery destination the home heard announcing
/// (1643).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameRow {
    /// The `lxmf.delivery` destination.
    pub address: [u8; 16],
    /// The name its announce carried, as it came; a device sanitises it as
    /// it does an announce's.
    pub name: String,
    /// When the home first and last heard it, unix seconds, the home's
    /// clock.
    pub first_seen: u64,
    pub last_seen: u64,
    /// Its hops when last heard; `None` when unknown.
    pub hops: Option<u32>,
}

/// One row of a `/pages` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Page(PageRow),
    Bookmark(BookmarkRow),
    Name(NameRow),
}

/// A row with the home's stamp of its last change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamped {
    pub stamp: u64,
    pub row: Row,
}

/// One `/pages` answer: the rows after the device's stamp, oldest change
/// first, and the stamp to ask with next.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Delta {
    pub stamp: u64,
    pub more: bool,
    pub rows: Vec<Row>,
}

impl Delta {
    /// The page rows' nodes, as lnomad's registry takes them.
    pub fn nodes(&self) -> impl Iterator<Item = &DiscoveredNode> {
        self.rows.iter().filter_map(|row| match row {
            Row::Page(page) => Some(&page.node),
            Row::Bookmark(_) | Row::Name(_) => None,
        })
    }

    /// The bookmark rows.
    pub fn bookmarks(&self) -> impl Iterator<Item = &BookmarkRow> {
        self.rows.iter().filter_map(|row| match row {
            Row::Bookmark(bookmark) => Some(bookmark),
            Row::Page(_) | Row::Name(_) => None,
        })
    }

    /// The name rows (1643).
    pub fn names(&self) -> impl Iterator<Item = &NameRow> {
        self.rows.iter().filter_map(|row| match row {
            Row::Name(name) => Some(name),
            Row::Page(_) | Row::Bookmark(_) => None,
        })
    }
}

/// What the home does with a node heard again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// A new row, or one that says something new: stamp it.
    Stamp(PageRow),
    /// Only its count rises.
    Count,
}

/// The rule for a node heard announcing (`heard`, its `first_seen` and
/// `last_seen` the hearing's time) against the row the home `kept`: a
/// new node, another name or other hops, or a hearing
/// [`PAGE_REFRESH_SECS`] after the kept one is a new stamp; anything else
/// only counts. A nameless announce never clears a kept name, as
/// lnomad's registry does not.
pub fn heard(kept: Option<&PageRow>, heard: &DiscoveredNode) -> Heard {
    let Some(kept) = kept else {
        return Heard::Stamp(PageRow {
            node: heard.clone(),
            count: 1,
        });
    };
    let name = heard.name.clone().or_else(|| kept.node.name.clone());
    let hops = heard.hops.or(kept.node.hops);
    let refreshed = heard.last_seen
        >= kept
            .node
            .last_seen
            .saturating_add(PAGE_REFRESH_SECS.unsigned_abs());
    if name == kept.node.name && hops == kept.node.hops && !refreshed {
        return Heard::Count;
    }
    Heard::Stamp(PageRow {
        node: DiscoveredNode {
            dest_hash: heard.dest_hash,
            name,
            first_seen: kept.node.first_seen.min(heard.first_seen),
            last_seen: kept.node.last_seen.max(heard.last_seen),
            hops,
        },
        count: kept.count.saturating_add(1),
    })
}

/// The rule for a peer heard announcing `heard` (its `first_seen` and
/// `last_seen` the hearing's time) against the name row the home `kept`
/// (1643): the row to stamp when it is new, says another name or other
/// hops, or comes [`NAME_REFRESH_SECS`] after the kept hearing; `None`
/// when it says nothing new. Unknown hops never clear known ones.
pub fn name_heard(kept: Option<&NameRow>, heard: &NameRow) -> Option<NameRow> {
    let Some(kept) = kept else {
        return Some(heard.clone());
    };
    let hops = heard.hops.or(kept.hops);
    let refreshed = heard.last_seen
        >= kept
            .last_seen
            .saturating_add(NAME_REFRESH_SECS.unsigned_abs());
    if heard.name == kept.name && hops == kept.hops && !refreshed {
        return None;
    }
    Some(NameRow {
        address: heard.address,
        name: heard.name.clone(),
        first_seen: kept.first_seen.min(heard.first_seen),
        last_seen: kept.last_seen.max(heard.last_seen),
        hops,
    })
}

/// Whether `incoming` replaces the `kept` row of the same URL: last
/// writer wins by stamp. On equal stamps the tombstone wins, then the
/// larger title, so every home and device decides one way.
pub fn bookmark_wins(kept: Option<&BookmarkRow>, incoming: &BookmarkRow) -> bool {
    let Some(kept) = kept else {
        return true;
    };
    match incoming.at.cmp(&kept.at) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Less => false,
        core::cmp::Ordering::Equal => {
            (incoming.deleted, &incoming.title) > (kept.deleted, &kept.title)
        }
    }
}

/// `/pages`: the device's stamp, and the most bytes the answer may take
/// when the medium is narrow.
pub fn pages_request(since: u64, page_bytes: Option<u64>) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 1 + usize::from(page_bytes.is_some()));
    msgpack::uint(&mut out, since);
    if let Some(bytes) = page_bytes {
        msgpack::uint(&mut out, bytes);
    }
    out
}

/// A `/pages` request: the stamp and the bound.
pub fn read_pages_request(data: &[u8]) -> Result<(u64, Option<u64>), Malformed> {
    let mut p = 0;
    let len = msgpack::array_len(data, &mut p)?;
    if len == 0 {
        return Err(Malformed);
    }
    let since = msgpack::read_uint(data, &mut p)?;
    let page = match len {
        1 => None,
        _ => Some(msgpack::read_uint(data, &mut p)?),
    };
    Ok((since, page))
}

fn row_out(out: &mut Vec<u8>, row: &Row) {
    match row {
        Row::Page(page) => {
            msgpack::array(out, 7);
            msgpack::uint(out, ROW_PAGE);
            msgpack::bin(out, &page.node.dest_hash);
            msgpack::string(out, page.node.name.as_deref().unwrap_or_default());
            msgpack::int(out, i64::try_from(page.node.first_seen).unwrap_or(i64::MAX));
            msgpack::int(out, i64::try_from(page.node.last_seen).unwrap_or(i64::MAX));
            msgpack::uint(out, page.node.hops.map_or(HOPS_UNKNOWN, u64::from));
            msgpack::uint(out, page.count);
        }
        Row::Bookmark(bookmark) => {
            msgpack::array(out, 5);
            msgpack::uint(out, ROW_BOOKMARK);
            bookmark_out(out, bookmark);
        }
        Row::Name(name) => {
            msgpack::array(out, 6);
            msgpack::uint(out, ROW_NAME);
            msgpack::bin(out, &name.address);
            msgpack::string(out, &name.name);
            msgpack::int(out, i64::try_from(name.first_seen).unwrap_or(i64::MAX));
            msgpack::int(out, i64::try_from(name.last_seen).unwrap_or(i64::MAX));
            msgpack::uint(out, name.hops.map_or(HOPS_UNKNOWN, u64::from));
        }
    }
}

fn bookmark_out(out: &mut Vec<u8>, bookmark: &BookmarkRow) {
    msgpack::string(out, &bookmark.url);
    msgpack::string(out, &bookmark.title);
    msgpack::int(out, bookmark.at);
    msgpack::bool(out, bookmark.deleted);
}

fn row_len(row: &Row) -> usize {
    let mut out = Vec::new();
    row_out(&mut out, row);
    out.len()
}

/// The answer's head before its rows, at its longest: the array of four,
/// the status, the stamp as a full `uint64`, `more`, and the rows' array
/// header as an `array32`.
const HEAD_MAX: usize = 1 + 1 + 9 + 1 + 5;

/// The answer to `/pages` from the `rows` changed after `since`, in stamp
/// order: as many as fit `bytes` (at least one), `more` when any stayed
/// behind or `behind` says the store held more than it read. The stamp is
/// the last row's that went, `since` when none did.
pub fn fit(rows: Vec<Stamped>, since: u64, bytes: usize, behind: bool) -> Delta {
    let mut delta = Delta {
        stamp: since,
        more: behind,
        rows: Vec::new(),
    };
    let mut len = HEAD_MAX;
    for stamped in rows {
        let row_bytes = row_len(&stamped.row);
        if !delta.rows.is_empty() && len.saturating_add(row_bytes) > bytes {
            delta.more = true;
            break;
        }
        len = len.saturating_add(row_bytes);
        delta.stamp = stamped.stamp;
        delta.rows.push(stamped.row);
    }
    delta
}

pub fn pages_answer(delta: &Delta) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 4);
    msgpack::uint(&mut out, 0);
    msgpack::uint(&mut out, delta.stamp);
    msgpack::bool(&mut out, delta.more);
    msgpack::array(&mut out, delta.rows.len());
    for row in &delta.rows {
        row_out(&mut out, row);
    }
    out
}

/// A `/pages` answer, or the status that refused it. A row of a kind this
/// program does not know is passed by, so a later home may add kinds.
pub fn read_pages_answer(data: &[u8]) -> Result<Result<Delta, Status>, Malformed> {
    let status = wire::read_status(data)?;
    if status != Status::Ok {
        return Ok(Err(status));
    }
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? < 4 {
        return Err(Malformed);
    }
    msgpack::read_uint(data, &mut p)?;
    let stamp = msgpack::read_uint(data, &mut p)?;
    let more = msgpack::read_bool(data, &mut p)?;
    let count = msgpack::array_len(data, &mut p)?;
    let mut rows = Vec::new();
    for _ in 0..count {
        let len = msgpack::array_len(data, &mut p)?;
        if len == 0 {
            return Err(Malformed);
        }
        let kind = msgpack::read_uint(data, &mut p)?;
        let mut taken = 1;
        match kind {
            ROW_PAGE if len >= 7 => {
                let dest_hash = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?)
                    .map_err(|_| Malformed)?;
                let name = lnomad::discovery::decode_node_name(
                    msgpack::read_str(data, &mut p)?.as_bytes(),
                );
                let first_seen = u64::try_from(msgpack::read_int(data, &mut p)?).unwrap_or(0);
                let last_seen = u64::try_from(msgpack::read_int(data, &mut p)?).unwrap_or(0);
                let hops = msgpack::read_uint(data, &mut p)?;
                let hops = u32::try_from(hops)
                    .ok()
                    .filter(|h| u64::from(*h) != HOPS_UNKNOWN);
                let count = msgpack::read_uint(data, &mut p)?;
                taken = 7;
                rows.push(Row::Page(PageRow {
                    node: DiscoveredNode {
                        dest_hash,
                        name,
                        first_seen,
                        last_seen,
                        hops,
                    },
                    count,
                }));
            }
            ROW_BOOKMARK if len >= 5 => {
                rows.push(Row::Bookmark(read_bookmark_fields(data, &mut p)?));
                taken = 5;
            }
            ROW_NAME if len >= 6 => {
                let address = <[u8; 16]>::try_from(msgpack::read_bin(data, &mut p)?)
                    .map_err(|_| Malformed)?;
                let name = String::from(msgpack::read_str(data, &mut p)?);
                let first_seen = u64::try_from(msgpack::read_int(data, &mut p)?).unwrap_or(0);
                let last_seen = u64::try_from(msgpack::read_int(data, &mut p)?).unwrap_or(0);
                let hops = u32::try_from(msgpack::read_uint(data, &mut p)?)
                    .ok()
                    .filter(|h| u64::from(*h) != HOPS_UNKNOWN);
                taken = 6;
                rows.push(Row::Name(NameRow {
                    address,
                    name,
                    first_seen,
                    last_seen,
                    hops,
                }));
            }
            _ => {}
        }
        for _ in taken..len {
            msgpack::skip(data, &mut p)?;
        }
    }
    Ok(Ok(Delta { stamp, more, rows }))
}

fn read_bookmark_fields(data: &[u8], p: &mut usize) -> Result<BookmarkRow, Malformed> {
    let url = String::from(msgpack::read_str(data, p)?);
    let title = String::from(msgpack::read_str(data, p)?);
    let at = msgpack::read_int(data, p)?;
    let deleted = msgpack::read_bool(data, p)?;
    if url.is_empty() {
        return Err(Malformed);
    }
    Ok(BookmarkRow {
        url,
        title,
        at,
        deleted,
    })
}

/// `/bookmark`: one bookmark set or cleared on a device.
pub fn bookmark_request(bookmark: &BookmarkRow) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::array(&mut out, 4);
    bookmark_out(&mut out, bookmark);
    out
}

pub fn read_bookmark_request(data: &[u8]) -> Result<BookmarkRow, Malformed> {
    let mut p = 0;
    if msgpack::array_len(data, &mut p)? < 4 {
        return Err(Malformed);
    }
    read_bookmark_fields(data, &mut p)
}

/// The bookmark changes between what a device held (`before`) and what it
/// holds now (`after`), stamped `at`: a new URL or a new title is a set, a
/// URL gone is a tombstone. What the device hands its home.
pub fn bookmark_changes(before: &Bookmarks, after: &Bookmarks, at: i64) -> Vec<BookmarkRow> {
    let mut changes = Vec::new();
    for bookmark in after.list() {
        let same = before
            .list()
            .iter()
            .any(|b| b.url == bookmark.url && b.title == bookmark.title);
        if !same {
            changes.push(BookmarkRow {
                url: bookmark.url.clone(),
                title: bookmark.title.clone(),
                at,
                deleted: false,
            });
        }
    }
    for bookmark in before.list() {
        if !after.contains(&bookmark.url) {
            changes.push(BookmarkRow {
                url: bookmark.url.clone(),
                title: bookmark.title.clone(),
                at,
                deleted: true,
            });
        }
    }
    changes
}

/// A device's side of what it shares with its home: the stamp of the rows
/// it holds, the bookmarks as the home last knew them, and the changes it
/// made that the home has not echoed yet, which a home row older than
/// them does not undo. A device without a home has none of this and keeps
/// its own list, as lnomad does.
#[derive(Debug, Clone, Default)]
pub struct Shared {
    /// The stamp to ask `/pages` with: the last answer's. Held in memory
    /// only, as lnomad's node list is, so a start asks for the whole list.
    pub stamp: u64,
    synced: Bookmarks,
    pending: BTreeMap<String, BookmarkRow>,
}

impl Shared {
    pub fn new() -> Self {
        Self::default()
    }

    /// The device's bookmarks as it loaded them: what it holds already,
    /// so only a later change goes to the home.
    pub fn loaded(&mut self, bookmarks: &Bookmarks) {
        self.synced = bookmarks.clone();
    }

    /// The device's bookmarks changed to `now` (the browser saved them):
    /// the rows for the home, stamped `at`, waiting until the home echoes
    /// them.
    pub fn local(&mut self, now: &Bookmarks, at: i64) -> Vec<BookmarkRow> {
        let rows = bookmark_changes(&self.synced, now, at);
        self.synced = now.clone();
        self.changed(&rows);
        rows
    }

    /// A standalone install's bookmarks handed to the home once (the
    /// import, [`import`]), stamped `at`, the time the file was written,
    /// so a newer word from another device still wins.
    pub fn import(&mut self, bookmarks: &Bookmarks, at: i64) -> Vec<BookmarkRow> {
        let rows = import(bookmarks, at);
        self.changed(&rows);
        rows
    }

    /// The device changed these; they go to the home.
    pub fn changed(&mut self, rows: &[BookmarkRow]) {
        for row in rows {
            let newer = self
                .pending
                .get(&row.url)
                .is_none_or(|kept| bookmark_wins(Some(kept), row));
            if newer {
                self.pending.insert(row.url.clone(), row.clone());
            }
        }
    }

    /// The home took `row`: it waits no longer, unless the device changed
    /// that URL again since.
    pub fn taken(&mut self, row: &BookmarkRow) {
        if self.pending.get(&row.url) == Some(row) {
            self.pending.remove(&row.url);
        }
    }

    /// What still waits for the home, oldest stamp first: handed again
    /// before every ask, since the home keeps a row it has once.
    pub fn pending(&self) -> Vec<BookmarkRow> {
        let mut rows: Vec<BookmarkRow> = self.pending.values().cloned().collect();
        rows.sort_by_key(|row| row.at);
        rows
    }

    /// Apply the home's bookmark rows to the device's `bookmarks`: a set
    /// adds the URL or renames it in place, a tombstone removes it; a row
    /// a pending change of the device's beats is passed by, and the home's
    /// echo of a pending change ends its wait. Whether the list changed.
    pub fn apply<'a>(
        &mut self,
        bookmarks: &mut Bookmarks,
        rows: impl IntoIterator<Item = &'a BookmarkRow>,
    ) -> bool {
        let mut changed = false;
        for row in rows {
            if let Some(pending) = self.pending.get(&row.url) {
                if pending == row {
                    self.pending.remove(&row.url);
                    continue;
                }
                if !bookmark_wins(Some(pending), row) {
                    continue;
                }
                self.pending.remove(&row.url);
            }
            changed |= put(bookmarks, row);
            put(&mut self.synced, row);
        }
        changed
    }
}

/// One home row into a bookmark list; whether it changed the list.
fn put(bookmarks: &mut Bookmarks, row: &BookmarkRow) -> bool {
    if row.deleted {
        return bookmarks.remove(&row.url);
    }
    let index = bookmarks.list().iter().position(|b| b.url == row.url);
    let bookmark = Bookmark {
        url: row.url.clone(),
        title: row.title.clone(),
    };
    match index {
        Some(index) if bookmarks.list()[index].title == row.title => false,
        Some(index) => {
            bookmarks.remove(&row.url);
            bookmarks.insert(index, bookmark);
            true
        }
        None => bookmarks.add(bookmark),
    }
}

/// lnomad's bookmarks of a standalone install, the import: each a set at
/// `at`, so a newer word on the same URL from another device still wins.
pub fn import(bookmarks: &Bookmarks, at: i64) -> Vec<BookmarkRow> {
    bookmark_changes(&Bookmarks::new(), bookmarks, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn node(byte: u8, name: Option<&str>, at: u64, hops: Option<u32>) -> DiscoveredNode {
        DiscoveredNode {
            dest_hash: [byte; 16],
            name: name.map(ToString::to_string),
            first_seen: at,
            last_seen: at,
            hops,
        }
    }

    fn mark(url: &str, title: &str, at: i64, deleted: bool) -> BookmarkRow {
        BookmarkRow {
            url: url.into(),
            title: title.into(),
            at,
            deleted,
        }
    }

    /// A node heard again within the hour with the same name and hops
    /// only counts; a new name, new hops or a hearing an hour on stamps; a
    /// nameless announce keeps the name.
    #[test]
    fn a_node_heard_again_stamps_only_when_it_says_something_new() {
        let first = match heard(None, &node(1, Some("Alpha"), 1000, Some(2))) {
            Heard::Stamp(row) => row,
            Heard::Count => panic!("a new node stamps"),
        };
        assert_eq!(first.count, 1);
        assert_eq!(
            heard(Some(&first), &node(1, Some("Alpha"), 1500, Some(2))),
            Heard::Count
        );
        assert_eq!(
            heard(Some(&first), &node(1, None, 1500, None)),
            Heard::Count,
            "a nameless announce of unknown hops says nothing new"
        );
        let renamed = match heard(Some(&first), &node(1, Some("Beta"), 1500, Some(2))) {
            Heard::Stamp(row) => row,
            Heard::Count => panic!("a new name stamps"),
        };
        assert_eq!(renamed.node.name.as_deref(), Some("Beta"));
        assert_eq!(renamed.node.first_seen, 1000);
        assert_eq!(renamed.node.last_seen, 1500);
        assert_eq!(renamed.count, 2);
        assert!(matches!(
            heard(Some(&first), &node(1, Some("Alpha"), 1000 + 3600, Some(2))),
            Heard::Stamp(_)
        ));
        assert!(matches!(
            heard(Some(&first), &node(1, Some("Alpha"), 1001, Some(3))),
            Heard::Stamp(_)
        ));
    }

    /// 1643: a client from before the name rows reads an answer that
    /// carries them as one without: the page and bookmark rows, the stamp
    /// and `more`, each name row passed by as a kind it does not know.
    /// The reader here is the 1579 reader's rule, kind by kind: a row whose
    /// kind it does not match is skipped whole.
    #[test]
    fn an_old_client_passes_the_name_rows_by() {
        let delta = Delta {
            stamp: 9,
            more: false,
            rows: vec![
                Row::Name(name_row(1, "Alpha", 100, Some(2))),
                Row::Page(PageRow {
                    node: node(2, Some("Page"), 100, Some(1)),
                    count: 1,
                }),
                Row::Name(name_row(3, "Gamma", 100, None)),
            ],
        };
        let bytes = pages_answer(&delta);
        // The old reader: the same loop with the name arm taken out.
        let mut p = 0;
        msgpack::array_len(&bytes, &mut p).expect("head");
        msgpack::read_uint(&bytes, &mut p).expect("status");
        msgpack::read_uint(&bytes, &mut p).expect("stamp");
        msgpack::read_bool(&bytes, &mut p).expect("more");
        let count = msgpack::array_len(&bytes, &mut p).expect("rows");
        let mut kept = Vec::new();
        for _ in 0..count {
            let len = msgpack::array_len(&bytes, &mut p).expect("row");
            let kind = msgpack::read_uint(&bytes, &mut p).expect("kind");
            kept.push(kind);
            for _ in 1..len {
                msgpack::skip(&bytes, &mut p).expect("field");
            }
        }
        assert_eq!(p, bytes.len(), "every row passed whole");
        assert_eq!(kept, [ROW_NAME, ROW_PAGE, ROW_NAME]);
    }

    /// A peer heard again within the hour with the same name and hops says
    /// nothing; a new name, new hops or a hearing an hour on stamps; an
    /// unknown hops keeps the known one.
    #[test]
    fn a_name_heard_again_stamps_only_when_it_says_something_new() {
        let first = name_heard(None, &name_row(1, "Alpha", 1000, Some(2))).expect("new");
        assert_eq!(
            name_heard(Some(&first), &name_row(1, "Alpha", 1500, Some(2))),
            None
        );
        assert_eq!(
            name_heard(Some(&first), &name_row(1, "Alpha", 1500, None)),
            None,
            "unknown hops say nothing new"
        );
        let renamed = name_heard(Some(&first), &name_row(1, "Beta", 1500, None)).expect("renamed");
        assert_eq!(renamed.name, "Beta");
        assert_eq!((renamed.first_seen, renamed.last_seen), (1000, 1500));
        assert_eq!(renamed.hops, Some(2));
        assert!(name_heard(Some(&first), &name_row(1, "Alpha", 1000 + 3600, Some(2))).is_some());
        assert!(name_heard(Some(&first), &name_row(1, "Alpha", 1001, Some(3))).is_some());
    }

    /// The newer stamp wins; on a tie the tombstone, then the larger
    /// title; the same row again changes nothing.
    #[test]
    fn the_newer_bookmark_wins_and_a_tie_goes_to_the_tombstone() {
        let set = mark("aa:/page/index.mu", "Home", 10, false);
        assert!(bookmark_wins(None, &set));
        assert!(!bookmark_wins(Some(&set), &set));
        assert!(bookmark_wins(
            Some(&set),
            &mark("aa:/page/index.mu", "Home", 11, true)
        ));
        assert!(!bookmark_wins(
            Some(&set),
            &mark("aa:/page/index.mu", "Home", 9, true)
        ));
        assert!(bookmark_wins(
            Some(&set),
            &mark("aa:/page/index.mu", "Home", 10, true)
        ));
        assert!(!bookmark_wins(
            Some(&mark("aa:/page/index.mu", "Home", 10, true)),
            &set
        ));
        assert!(bookmark_wins(
            Some(&set),
            &mark("aa:/page/index.mu", "Z", 10, false)
        ));
    }

    fn name_row(byte: u8, name: &str, at: u64, hops: Option<u32>) -> NameRow {
        NameRow {
            address: [byte; 16],
            name: name.into(),
            first_seen: at,
            last_seen: at,
            hops,
        }
    }

    /// Request and answer round trip, the three row kinds, an unknown hops
    /// and an empty name; a row of an unknown kind is passed by.
    #[test]
    fn a_pages_answer_round_trips_and_passes_an_unknown_row_by() {
        assert_eq!(read_pages_request(&pages_request(7, None)), Ok((7, None)));
        assert_eq!(
            read_pages_request(&pages_request(7, Some(400))),
            Ok((7, Some(400)))
        );
        let delta = Delta {
            stamp: 42,
            more: true,
            rows: vec![
                Row::Page(PageRow {
                    node: node(3, Some("Gamma"), 100, Some(4)),
                    count: 9,
                }),
                Row::Page(PageRow {
                    node: node(4, None, 200, None),
                    count: 1,
                }),
                Row::Bookmark(mark("03:/page/index.mu", "Gamma home", 5, false)),
                Row::Bookmark(mark("04:/page/x.mu", "", 6, true)),
                Row::Name(name_row(5, "Delta", 300, Some(1))),
                Row::Name(name_row(6, "", 400, None)),
            ],
        };
        let bytes = pages_answer(&delta);
        assert_eq!(read_pages_answer(&bytes), Ok(Ok(delta.clone())));
        // A later kind, `[9, "x"]`, spliced in front of the rows.
        let mut later = Vec::new();
        msgpack::array(&mut later, 4);
        msgpack::uint(&mut later, 0);
        msgpack::uint(&mut later, 42);
        msgpack::bool(&mut later, true);
        msgpack::array(&mut later, delta.rows.len() + 1);
        msgpack::array(&mut later, 2);
        msgpack::uint(&mut later, 9);
        msgpack::string(&mut later, "x");
        for row in &delta.rows {
            row_out(&mut later, row);
        }
        assert_eq!(read_pages_answer(&later), Ok(Ok(delta.clone())));
        assert_eq!(delta.names().count(), 2);
        assert_eq!(delta.nodes().count(), 2);
        assert_eq!(delta.bookmarks().count(), 2);
        assert_eq!(
            read_pages_answer(&wire::status_answer(Status::NotAdmitted)),
            Ok(Err(Status::NotAdmitted))
        );
        let row = mark("aa:/", "t", 1, false);
        assert_eq!(read_bookmark_request(&bookmark_request(&row)), Ok(row));
    }

    /// The fit takes rows in stamp order until the bound, at least one,
    /// says more, and its stamp is the last row's that went; the answer it
    /// makes stays within the bound.
    #[test]
    fn the_fit_cuts_at_the_bound_and_stamps_the_last_row_sent() {
        let rows: Vec<Stamped> = (1..=50u8)
            .map(|i| Stamped {
                stamp: u64::from(i) * 10,
                row: Row::Page(PageRow {
                    node: node(i, Some("A node with a name"), 1_800_000_000, Some(3)),
                    count: 1,
                }),
            })
            .collect();
        let delta = fit(rows.clone(), 0, 400, false);
        assert!(delta.more);
        assert!(!delta.rows.is_empty() && delta.rows.len() < 50);
        assert_eq!(delta.stamp, delta.rows.len() as u64 * 10);
        assert!(
            pages_answer(&delta).len() <= 400,
            "{}",
            pages_answer(&delta).len()
        );
        let one = fit(rows.clone(), 0, 1, false);
        assert_eq!(one.rows.len(), 1, "at least one row");
        let all = fit(rows, 0, usize::MAX, false);
        assert_eq!((all.rows.len(), all.more, all.stamp), (50, false, 500));
        let none = fit(Vec::new(), 77, 400, false);
        assert_eq!((none.rows.len(), none.more, none.stamp), (0, false, 77));
    }

    /// A device's own change waits until the home took it, and a home row
    /// older than it does not undo it; a newer one does. A tombstone
    /// removes, a set renames in place.
    #[test]
    fn a_pending_change_beats_an_older_home_row() {
        let mut bookmarks = Bookmarks::new();
        bookmarks.add(Bookmark {
            url: "aa:/".into(),
            title: "A".into(),
        });
        bookmarks.add(Bookmark {
            url: "bb:/".into(),
            title: "B".into(),
        });
        let before = bookmarks.clone();
        bookmarks.add(Bookmark {
            url: "cc:/".into(),
            title: "C".into(),
        });
        bookmarks.remove("bb:/");
        let mut shared = Shared::new();
        shared.loaded(&before);
        let changes = shared.local(&bookmarks, 100);
        assert_eq!(
            changes,
            vec![mark("cc:/", "C", 100, false), mark("bb:/", "B", 100, true)]
        );
        assert!(shared.local(&bookmarks, 101).is_empty(), "said once");
        // The home says, from before: cc cleared at 50, bb set at 60.
        let older = [mark("cc:/", "C", 50, true), mark("bb:/", "B", 60, false)];
        assert!(!shared.apply(&mut bookmarks, &older));
        assert!(bookmarks.contains("cc:/") && !bookmarks.contains("bb:/"));
        // Another device renamed aa and cleared cc later.
        let newer = [mark("aa:/", "A2", 200, false), mark("cc:/", "C", 200, true)];
        assert!(shared.apply(&mut bookmarks, &newer));
        assert_eq!(bookmarks.list()[0].title, "A2", "renamed in place");
        assert!(!bookmarks.contains("cc:/"));
        assert_eq!(shared.pending(), vec![mark("bb:/", "B", 100, true)]);
        // The home's echo of the device's own row ends its wait and
        // changes nothing.
        assert!(!shared.apply(&mut bookmarks, &[mark("bb:/", "B", 100, true)]));
        assert!(shared.pending().is_empty());
        // What the home applied is no local change.
        assert!(shared.local(&bookmarks, 300).is_empty());
    }

    /// lnomad's file is the import: every bookmark a set.
    #[test]
    fn lnomads_bookmarks_import_as_sets() {
        let mut bookmarks = Bookmarks::new();
        bookmarks.add(Bookmark {
            url: "aa:/".into(),
            title: "A".into(),
        });
        assert_eq!(import(&bookmarks, 5), vec![mark("aa:/", "A", 5, false)]);
    }
}
