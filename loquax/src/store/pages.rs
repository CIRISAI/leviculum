//! The NomadNet pages a home heard and its identity's bookmarks, in the
//! store (1579, [`crate::home::pages`]).
//!
//! | table | columns | why |
//! |---|---|---|
//! | `home_pages` | dest, name, first_seen, last_seen, hops, count, stamp | a node the home heard announcing `nomadnetwork.node` |
//! | `home_bookmarks` | url, title, at, deleted, stamp | a bookmark of the identity, or its tombstone |
//! | `home_names` | address, name, first_seen, last_seen, hops, stamp | an LXMF address the home heard announcing a name (1643) |
//!
//! Every row is the owner's (`identity`), as every other table's. `stamp`
//! is one counter over the three tables: a row takes the next one whenever it
//! says something new, and a device asks for the rows after its own. The
//! row just written is never the one a cap evicts, so the newest stamp is
//! always held and the counter never runs back.

use lnomad::discovery::DiscoveredNode;
use rusqlite::{params, OptionalExtension, Transaction};

use super::{failed, Store, StoreError};
use crate::home::pages::{self, BookmarkRow, Heard, NameRow, PageRow, Row, Stamped};

/// The newest stamp in the store, 0 for none.
fn newest_in(tx: &rusqlite::Connection, identity: &[u8; 16]) -> rusqlite::Result<u64> {
    let newest: i64 = tx.query_row(
        "SELECT MAX(COALESCE((SELECT MAX(stamp) FROM home_pages WHERE identity = ?1), 0), \
                    COALESCE((SELECT MAX(stamp) FROM home_bookmarks WHERE identity = ?1), 0), \
                    COALESCE((SELECT MAX(stamp) FROM home_names WHERE identity = ?1), 0))",
        params![&identity[..]],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(newest).unwrap_or(0))
}

fn as_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn page_of(
    dest: Vec<u8>,
    name: Option<String>,
    first_seen: i64,
    last_seen: i64,
    hops: Option<i64>,
    count: i64,
) -> Option<PageRow> {
    Some(PageRow {
        node: DiscoveredNode {
            dest_hash: <[u8; 16]>::try_from(dest.as_slice()).ok()?,
            name,
            first_seen: u64::try_from(first_seen).unwrap_or(0),
            last_seen: u64::try_from(last_seen).unwrap_or(0),
            hops: hops.and_then(|h| u32::try_from(h).ok()),
        },
        count: u64::try_from(count).unwrap_or(0),
    })
}

fn name_of(
    address: Vec<u8>,
    name: String,
    first_seen: i64,
    last_seen: i64,
    hops: Option<i64>,
) -> Option<NameRow> {
    Some(NameRow {
        address: <[u8; 16]>::try_from(address.as_slice()).ok()?,
        name,
        first_seen: u64::try_from(first_seen).unwrap_or(0),
        last_seen: u64::try_from(last_seen).unwrap_or(0),
        hops: hops.and_then(|h| u32::try_from(h).ok()),
    })
}

fn kept_page(
    tx: &Transaction<'_>,
    identity: &[u8; 16],
    dest: &[u8; 16],
) -> rusqlite::Result<Option<PageRow>> {
    tx.query_row(
        "SELECT dest, name, first_seen, last_seen, hops, count FROM home_pages \
         WHERE identity = ?1 AND dest = ?2",
        params![&identity[..], &dest[..]],
        |row| {
            Ok(page_of(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )
    .optional()
    .map(Option::flatten)
}

impl Store {
    /// A node the home heard announcing its pages (1579): its row's new
    /// stamp when the hearing said something new ([`pages::heard`]),
    /// `None` when it only counted. Past [`pages::PAGES_CAP`] rows the
    /// one heard longest ago goes, never this one.
    pub fn page_heard(&mut self, node: &DiscoveredNode) -> Result<Option<u64>, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let kept = kept_page(&tx, &identity, &node.dest_hash).map_err(|e| failed(&path, e))?;
        let stamp = match pages::heard(kept.as_ref(), node) {
            Heard::Count => {
                tx.execute(
                    "UPDATE home_pages SET count = count + 1 WHERE identity = ?1 AND dest = ?2",
                    params![&identity[..], &node.dest_hash[..]],
                )
                .map_err(|e| failed(&path, e))?;
                None
            }
            Heard::Stamp(row) => {
                let stamp = newest_in(&tx, &identity)
                    .map_err(|e| failed(&path, e))?
                    .saturating_add(1);
                tx.execute(
                    "INSERT OR REPLACE INTO home_pages \
                     (identity, dest, name, first_seen, last_seen, hops, count, stamp) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        &identity[..],
                        &row.node.dest_hash[..],
                        row.node.name,
                        as_i64(row.node.first_seen),
                        as_i64(row.node.last_seen),
                        row.node.hops.map(i64::from),
                        as_i64(row.count),
                        as_i64(stamp)
                    ],
                )
                .map_err(|e| failed(&path, e))?;
                let cap = i64::try_from(pages::PAGES_CAP).unwrap_or(i64::MAX);
                tx.execute(
                    "DELETE FROM home_pages WHERE identity = ?1 AND dest IN \
                     (SELECT dest FROM home_pages WHERE identity = ?1 AND dest <> ?2 \
                      ORDER BY last_seen DESC, stamp DESC LIMIT -1 OFFSET ?3)",
                    params![&identity[..], &row.node.dest_hash[..], cap - 1],
                )
                .map_err(|e| failed(&path, e))?;
                Some(stamp)
            }
        };
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(stamp)
    }

    /// An LXMF address the home heard announcing a name (1643): its row's
    /// new stamp when the hearing said something new
    /// ([`pages::name_heard`]), `None` when it said nothing. Past
    /// [`pages::NAMES_CAP`] rows the one heard longest ago goes, never this
    /// one. A name row wakes nobody: it rides in the next `/pages`.
    pub fn name_heard(&mut self, heard: &NameRow) -> Result<Option<u64>, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let kept = tx
            .query_row(
                "SELECT address, name, first_seen, last_seen, hops FROM home_names \
                 WHERE identity = ?1 AND address = ?2",
                params![&identity[..], &heard.address[..]],
                |row| {
                    Ok(name_of(
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| failed(&path, e))?
            .flatten();
        let Some(row) = pages::name_heard(kept.as_ref(), heard) else {
            return Ok(None);
        };
        let stamp = newest_in(&tx, &identity)
            .map_err(|e| failed(&path, e))?
            .saturating_add(1);
        tx.execute(
            "INSERT OR REPLACE INTO home_names \
             (identity, address, name, first_seen, last_seen, hops, stamp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &identity[..],
                &row.address[..],
                row.name,
                as_i64(row.first_seen),
                as_i64(row.last_seen),
                row.hops.map(i64::from),
                as_i64(stamp)
            ],
        )
        .map_err(|e| failed(&path, e))?;
        if kept.is_none() {
            let cap = i64::try_from(pages::NAMES_CAP).unwrap_or(i64::MAX);
            tx.execute(
                "DELETE FROM home_names WHERE identity = ?1 AND address IN \
                 (SELECT address FROM home_names WHERE identity = ?1 AND address <> ?2 \
                  ORDER BY last_seen DESC, stamp DESC LIMIT -1 OFFSET ?3)",
                params![&identity[..], &row.address[..], cap - 1],
            )
            .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(Some(stamp))
    }

    /// How many name rows the home keeps (1643).
    pub fn names_kept(&self) -> Result<usize, StoreError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM home_names WHERE identity = ?1",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// A bookmark a device set or cleared (1579): its new stamp when it
    /// won against the kept row ([`pages::bookmark_wins`]), `None` when
    /// the kept one is newer. Past [`pages::BOOKMARKS_CAP`] rows the
    /// oldest tombstone goes first, then the oldest bookmark, never this
    /// one.
    pub fn bookmark_put(&mut self, bookmark: &BookmarkRow) -> Result<Option<u64>, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        let kept: Option<BookmarkRow> = tx
            .query_row(
                "SELECT url, title, at, deleted FROM home_bookmarks \
                 WHERE identity = ?1 AND url = ?2",
                params![&identity[..], &bookmark.url],
                |row| {
                    Ok(BookmarkRow {
                        url: row.get(0)?,
                        title: row.get(1)?,
                        at: row.get(2)?,
                        deleted: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
            .map_err(|e| failed(&path, e))?;
        if !pages::bookmark_wins(kept.as_ref(), bookmark) {
            return Ok(None);
        }
        let stamp = newest_in(&tx, &identity)
            .map_err(|e| failed(&path, e))?
            .saturating_add(1);
        tx.execute(
            "INSERT OR REPLACE INTO home_bookmarks (identity, url, title, at, deleted, stamp) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &identity[..],
                &bookmark.url,
                &bookmark.title,
                bookmark.at,
                i64::from(bookmark.deleted),
                as_i64(stamp)
            ],
        )
        .map_err(|e| failed(&path, e))?;
        let cap = i64::try_from(pages::BOOKMARKS_CAP).unwrap_or(i64::MAX);
        tx.execute(
            "DELETE FROM home_bookmarks WHERE identity = ?1 AND url IN \
             (SELECT url FROM home_bookmarks WHERE identity = ?1 AND url <> ?2 \
              ORDER BY deleted ASC, at DESC LIMIT -1 OFFSET ?3)",
            params![&identity[..], &bookmark.url, cap - 1],
        )
        .map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))?;
        Ok(Some(stamp))
    }

    /// Every page row kept, heard last first (1632): what a device lists
    /// again at its start.
    pub fn pages_heard(&self) -> Result<Vec<DiscoveredNode>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT dest, name, first_seen, last_seen, hops, count FROM home_pages \
                 WHERE identity = ?1 ORDER BY last_seen DESC, stamp DESC",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], |row| {
                Ok(page_of(
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .map_err(|e| self.failed(e))?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(page) = row.map_err(|e| self.failed(e))? {
                out.push(page.node);
            }
        }
        Ok(out)
    }

    /// The newest stamp of the pages, bookmarks and names, 0 when there are
    /// none.
    pub fn pages_newest(&self) -> Result<u64, StoreError> {
        newest_in(&self.conn, &self.identity).map_err(|e| self.failed(e))
    }

    /// The page, bookmark and name rows with a stamp after `since`, oldest change
    /// first, at most `limit`, and whether more follow.
    pub fn pages_since(
        &self,
        since: u64,
        limit: usize,
    ) -> Result<(Vec<Stamped>, bool), StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT stamp, 0, dest, name, first_seen, last_seen, hops, count, NULL, NULL \
                 FROM home_pages WHERE identity = ?1 AND stamp > ?2 \
                 UNION ALL \
                 SELECT stamp, 1, NULL, title, at, deleted, NULL, NULL, url, NULL \
                 FROM home_bookmarks WHERE identity = ?1 AND stamp > ?2 \
                 UNION ALL \
                 SELECT stamp, 2, address, name, first_seen, last_seen, hops, NULL, NULL, NULL \
                 FROM home_names WHERE identity = ?1 AND stamp > ?2 \
                 ORDER BY 1 LIMIT ?3",
            )
            .map_err(|e| self.failed(e))?;
        let asked = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        let rows = statement
            .query_map(params![&self.identity[..], as_i64(since), asked], |row| {
                let stamp: i64 = row.get(0)?;
                let kind: i64 = row.get(1)?;
                let stamped = match kind {
                    0 => page_of(
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    )
                    .map(Row::Page),
                    2 => name_of(
                        row.get(2)?,
                        row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    )
                    .map(Row::Name),
                    _ => Some(Row::Bookmark(BookmarkRow {
                        url: row.get(8)?,
                        title: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        at: row.get(4)?,
                        deleted: row.get::<_, i64>(5)? != 0,
                    })),
                };
                Ok(stamped.map(|row| Stamped {
                    stamp: u64::try_from(stamp).unwrap_or(0),
                    row,
                }))
            })
            .map_err(|e| self.failed(e))?;
        let mut out = Vec::new();
        for row in rows {
            if let Some(stamped) = row.map_err(|e| self.failed(e))? {
                out.push(stamped);
            }
        }
        let more = out.len() > limit;
        out.truncate(limit);
        Ok((out, more))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: [u8; 16] = [0x11; 16];

    fn node(byte: u8, name: &str, at: u64) -> DiscoveredNode {
        DiscoveredNode {
            dest_hash: [byte; 16],
            name: Some(name.to_string()),
            first_seen: at,
            last_seen: at,
            hops: Some(2),
        }
    }

    fn mark(url: &str, at: i64, deleted: bool) -> BookmarkRow {
        BookmarkRow {
            url: url.into(),
            title: "T".into(),
            at,
            deleted,
        }
    }

    /// The full list from stamp 0, then only what changed after the stamp
    /// the first ask ended on: a renamed node and a bookmark, not the node
    /// that only counted.
    #[test]
    fn the_first_ask_brings_the_list_and_the_next_the_delta() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("store");
        for i in 1..=5u8 {
            assert!(store
                .page_heard(&node(i, "N", 1000))
                .expect("heard")
                .is_some());
        }
        let (all, more) = store.pages_since(0, 100).expect("since");
        assert_eq!((all.len(), more), (5, false));
        let stamp = all.last().map(|s| s.stamp).expect("a row");
        assert_eq!(store.pages_newest().expect("newest"), stamp);
        assert_eq!(store.page_heard(&node(1, "N", 1200)).expect("again"), None);
        assert!(store
            .page_heard(&node(2, "Renamed", 1200))
            .expect("renamed")
            .is_some());
        assert!(store
            .bookmark_put(&mark("01:/", 5, false))
            .expect("put")
            .is_some());
        let (delta, _) = store.pages_since(stamp, 100).expect("delta");
        assert_eq!(delta.len(), 2, "{delta:?}");
        match &delta[0].row {
            Row::Page(row) => {
                assert_eq!(row.node.name.as_deref(), Some("Renamed"));
                assert_eq!(row.count, 2);
            }
            other => panic!("a page first: {other:?}"),
        }
        assert!(matches!(&delta[1].row, Row::Bookmark(b) if b.url == "01:/"));
        let (paged, more) = store.pages_since(0, 3).expect("paged");
        assert_eq!((paged.len(), more), (3, true));
        // The count kept rising without a stamp.
        let (all, _) = store.pages_since(0, 100).expect("all");
        let one = all.iter().find_map(|s| match &s.row {
            Row::Page(row) if row.node.dest_hash == [1; 16] => Some(row.count),
            _ => None,
        });
        assert_eq!(one, Some(2));
    }

    /// Past the cap the node heard longest ago goes, the one heard now
    /// never, and the stamp never runs back.
    #[test]
    fn past_the_cap_the_node_heard_longest_ago_goes() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("store");
        let cap = pages::PAGES_CAP;
        for i in 0..cap {
            let mut n = node(0, "N", 10_000 + i as u64);
            n.dest_hash[..8].copy_from_slice(&(i as u64).to_be_bytes());
            store.page_heard(&n).expect("heard");
        }
        let newest = store.pages_newest().expect("newest");
        // One heard long ago, which is still the one heard now.
        let late = node(0xee, "Late", 1);
        let stamp = store.page_heard(&late).expect("late").expect("stamped");
        assert!(stamp > newest);
        let (all, _) = store.pages_since(0, cap + 10).expect("all");
        assert_eq!(all.len(), cap);
        let dests: Vec<[u8; 16]> = all
            .iter()
            .filter_map(|s| match &s.row {
                Row::Page(row) => Some(row.node.dest_hash),
                Row::Bookmark(_) | Row::Name(_) => None,
            })
            .collect();
        assert!(dests.contains(&[0xee; 16]), "the one heard now stays");
        let mut first = [0u8; 16];
        first[..8].copy_from_slice(&0u64.to_be_bytes());
        assert!(!dests.contains(&first), "the oldest heard went");
        assert_eq!(store.pages_newest().expect("newest"), stamp);
    }

    fn name(i: u32, name: &str, at: u64) -> NameRow {
        let mut address = [0u8; 16];
        address[..4].copy_from_slice(&i.to_be_bytes());
        NameRow {
            address,
            name: name.into(),
            first_seen: at,
            last_seen: at,
            hops: Some(2),
        }
    }

    /// 1643: a name row takes the stamp the pages and bookmarks share, a
    /// hearing that says nothing new takes none, and the next ask after
    /// the stamp brings only the rename.
    #[test]
    fn a_name_rides_the_shared_stamp_and_only_a_change_takes_one() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("store");
        let page = store
            .page_heard(&node(1, "N", 1000))
            .expect("page")
            .expect("stamped");
        let first = store
            .name_heard(&name(1, "Alice", 1000))
            .expect("name")
            .expect("stamped");
        assert!(first > page);
        assert_eq!(
            store.name_heard(&name(1, "Alice", 1200)).expect("again"),
            None
        );
        assert_eq!(store.pages_newest().expect("newest"), first);
        let renamed = store
            .name_heard(&name(1, "Alice B.", 1300))
            .expect("renamed")
            .expect("stamped");
        let (delta, more) = store.pages_since(first, 100).expect("delta");
        assert!(!more);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].stamp, renamed);
        match &delta[0].row {
            Row::Name(row) => {
                assert_eq!(row.name, "Alice B.");
                assert_eq!((row.first_seen, row.last_seen), (1000, 1300));
            }
            other => panic!("a name: {other:?}"),
        }
        let (all, _) = store.pages_since(0, 100).expect("all");
        assert_eq!(all.len(), 2, "one page, one name: {all:?}");
    }

    /// 1643: past [`pages::NAMES_CAP`] the address heard longest ago goes,
    /// the one heard now never, even when its own hearing is the oldest;
    /// a name heard again is no new row and evicts nobody.
    #[test]
    fn past_the_name_cap_the_address_heard_longest_ago_goes() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("store");
        let cap = pages::NAMES_CAP;
        for i in 0..cap {
            store
                .name_heard(&name(i as u32, "N", 10_000 + i as u64))
                .expect("heard");
        }
        assert_eq!(store.names_kept().expect("count"), cap);
        // Heard again a day on: a new stamp, the same row count.
        store
            .name_heard(&name(0, "N", 200_000))
            .expect("again")
            .expect("refreshed");
        assert_eq!(store.names_kept().expect("count"), cap);
        let late = name(0xffff_ff00, "Late", 1);
        store.name_heard(&late).expect("late").expect("stamped");
        assert_eq!(store.names_kept().expect("count"), cap);
        let (all, _) = store.pages_since(0, cap + 10).expect("all");
        let kept: Vec<[u8; 16]> = all
            .iter()
            .filter_map(|s| match &s.row {
                Row::Name(row) => Some(row.address),
                _ => None,
            })
            .collect();
        assert_eq!(kept.len(), cap);
        assert!(kept.contains(&late.address), "the one heard now stays");
        assert!(kept.contains(&name(0, "N", 0).address), "refreshed, kept");
        assert!(
            !kept.contains(&name(1, "N", 0).address),
            "the one heard longest ago went"
        );
    }

    /// A bookmark's newer word wins, an older one is refused, and a
    /// tombstone comes back to the next ask.
    #[test]
    fn the_newer_bookmark_wins_in_the_store_and_its_tombstone_travels() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("store");
        assert!(store
            .bookmark_put(&mark("a:/", 10, false))
            .expect("set")
            .is_some());
        assert_eq!(
            store.bookmark_put(&mark("a:/", 9, true)).expect("older"),
            None
        );
        let stamp = store.pages_newest().expect("newest");
        assert!(store
            .bookmark_put(&mark("a:/", 11, true))
            .expect("cleared")
            .is_some());
        let (delta, _) = store.pages_since(stamp, 10).expect("delta");
        assert_eq!(delta.len(), 1);
        assert!(matches!(&delta[0].row, Row::Bookmark(b) if b.deleted && b.at == 11));
    }

    /// Past the bookmark cap the oldest tombstone goes before any
    /// bookmark.
    #[test]
    fn past_the_bookmark_cap_a_tombstone_goes_first() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), US).expect("store");
        store
            .bookmark_put(&mark("gone:/", 1, true))
            .expect("tombstone");
        for i in 0..pages::BOOKMARKS_CAP {
            store
                .bookmark_put(&mark(&format!("{i}:/"), 100 + i as i64, false))
                .expect("set");
        }
        let (all, _) = store.pages_since(0, 4096).expect("all");
        assert_eq!(all.len(), pages::BOOKMARKS_CAP);
        assert!(!all
            .iter()
            .any(|s| matches!(&s.row, Row::Bookmark(b) if b.url == "gone:/")));
    }
}
