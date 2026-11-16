//! loquax's own table of links from LXMF addresses to the phone's address
//! book (1139; `docs/drafts/loquax-android-contacts-2026-10.md`, model c):
//! the contact's `LOOKUP_KEY` and last `_ID`, the name the link copied, and
//! the addresses whose banner the user answered "Not now". loquax's word is
//! the truth; the address book is a presentation layer. Nothing of it goes
//! to the home node.
//!
//! The file is `address-book` in the app's directory, one line each:
//!
//! ```text
//! since <unix seconds>
//! link <address> <contact id> <lookup key> <name>
//! not-now <address>
//! ```
//!
//! `since` is when this device began (1448, walk finding 12): the banner
//! asks only for a person whose first word came after it, so a history a
//! new device pulls from its mailbox asks nothing in every chat.
//!
//! The lookup key is percent-encoded where it holds a space or a `%`. The
//! copied thumbnail is `photos/<address>` beside it, the bytes the address
//! book gave.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use loquax::address::{parse, to_hex};

/// The table's file under the app's directory.
pub const BOOK_FILE: &str = "address-book";
/// The thumbnails' directory under it.
pub const PHOTOS_DIR: &str = "photos";

/// One address's link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The contact's `_ID` when it was linked: the fast path.
    pub contact_id: i64,
    /// The contact's `LOOKUP_KEY`: what survives a merge or a sync.
    pub lookup: String,
    /// The contact's name, sanitised, as the link copied it.
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    pub links: BTreeMap<[u8; 16], Link>,
    pub not_now: BTreeSet<[u8; 16]>,
    /// When this device began, in Unix seconds; `None` in a file from
    /// before 1448, which the session stamps on its first start.
    pub since: Option<i64>,
}

impl Book {
    /// The addresses linked to the contact `lookup`.
    pub fn of_contact(&self, lookup: &str) -> Vec<[u8; 16]> {
        self.links
            .iter()
            .filter(|(_, link)| link.lookup == lookup)
            .map(|(peer, _)| *peer)
            .collect()
    }

    /// Whether the banner asks to save a person whose first word, by its
    /// writer's clock, is `first` (1448): only for a person met after
    /// this device began. A word without a time asks nothing, nor does a
    /// device without a beginning, which the session stamps at start.
    pub fn met_here(&self, first: Option<i64>) -> bool {
        matches!((self.since, first), (Some(since), Some(first)) if first >= since)
    }

    /// The table as its file says it.
    pub fn text(&self) -> String {
        let mut out = String::from("# Loquax's links to the phone's address book\n");
        if let Some(since) = self.since {
            out.push_str(&format!("since {since}\n"));
        }
        for (peer, link) in &self.links {
            out.push_str(&format!(
                "link {} {} {} {}\n",
                to_hex(peer),
                link.contact_id,
                escape(&link.lookup),
                link.name
            ));
        }
        for peer in &self.not_now {
            out.push_str(&format!("not-now {}\n", to_hex(peer)));
        }
        out
    }
}

/// Read the table's text. A line this version does not read is skipped
/// with a warning: the file is loquax's own, and a link lost is one tap
/// to make again, where a refused start is the whole app.
pub fn parse_book(text: &str) -> Book {
    let mut book = Book::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(secs) = line.strip_prefix("since ") {
            match secs.trim().parse() {
                Ok(since) => book.since = Some(since),
                Err(_) => tracing::warn!("{BOOK_FILE}:{}: not a time", index + 1),
            }
            continue;
        }
        let mut words = line.splitn(5, ' ');
        let verb = words.next();
        let peer = words.next().and_then(|hex| parse(hex).ok());
        match (verb, peer) {
            (Some("not-now"), Some(peer)) => {
                book.not_now.insert(peer);
            }
            (Some("link"), Some(peer)) => {
                let id = words.next().and_then(|id| id.parse().ok());
                let lookup = words.next().map(unescape);
                let name = words.next().and_then(loquax::names::sanitise);
                match (id, lookup, name) {
                    (Some(contact_id), Some(lookup), Some(name)) if !lookup.is_empty() => {
                        book.links.insert(
                            peer,
                            Link {
                                contact_id,
                                lookup,
                                name,
                            },
                        );
                    }
                    _ => tracing::warn!("{BOOK_FILE}:{}: not a link", index + 1),
                }
            }
            _ => tracing::warn!("{BOOK_FILE}:{}: not read", index + 1),
        }
    }
    book
}

/// `home`'s table; none for a missing or unreadable file, said in the log.
pub fn load(home: &Path) -> Book {
    match std::fs::read_to_string(home.join(BOOK_FILE)) {
        Ok(text) => parse_book(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Book::default(),
        Err(error) => {
            tracing::warn!("{BOOK_FILE}: {error}");
            Book::default()
        }
    }
}

/// Write `home`'s table through a rename.
pub fn save(home: &Path, book: &Book) -> Result<(), String> {
    loquax::home::client::write_atomically(&home.join(BOOK_FILE), &book.text())
}

/// Where `peer`'s copied thumbnail lives.
pub fn photo_path(home: &Path, peer: &[u8; 16]) -> PathBuf {
    home.join(PHOTOS_DIR).join(to_hex(peer))
}

/// Keep `bytes` as `peer`'s thumbnail, or drop the one kept for `None`.
pub fn keep_photo(home: &Path, peer: &[u8; 16], bytes: Option<&[u8]>) -> Result<(), String> {
    let path = photo_path(home, peer);
    let words = |error: std::io::Error| format!("{}: {error}", path.display());
    match bytes {
        Some(bytes) if !bytes.is_empty() => {
            std::fs::create_dir_all(home.join(PHOTOS_DIR)).map_err(words)?;
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, bytes).map_err(words)?;
            std::fs::rename(&tmp, &path).map_err(words)
        }
        _ => match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(words(error)),
            _ => Ok(()),
        },
    }
}

/// `peer`'s thumbnail's path, when one is kept.
pub fn photo(home: &Path, peer: &[u8; 16]) -> Option<String> {
    let path = photo_path(home, peer);
    path.is_file().then(|| path.display().to_string())
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '%' => out.push_str("%25"),
            ch if ch.is_whitespace() => {
                let mut buf = [0u8; 4];
                for byte in ch.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: [u8; 16] = [0x4f; 16];
    const BOB: [u8; 16] = [0xb0; 16];

    #[test]
    fn the_table_round_trips_through_its_text() {
        let mut book = Book::default();
        book.links.insert(
            ALICE,
            Link {
                contact_id: 42,
                lookup: "0r1-2C3E% odd\tkey".to_string(),
                name: "Alice Schmidt".to_string(),
            },
        );
        book.not_now.insert(BOB);
        book.since = Some(1_790_000_000);
        let text = book.text();
        assert!(text.contains("%25%20odd%09key"), "{text}");
        assert_eq!(parse_book(&text), book);
        assert_eq!(book.of_contact("0r1-2C3E% odd\tkey"), [ALICE]);
    }

    /// A line that is not one is skipped, the others stand; a name is
    /// sanitised as an announced one.
    #[test]
    fn a_bad_line_costs_only_itself() {
        let alice = to_hex(&ALICE);
        let text = format!(
            "frobnicate\nlink nothex 1 k Bob\nlink {alice} x k Alice\nlink {alice} 7 k\n\
             not-now {}\nlink {alice} 7 key Alice \u{202e}Schmidt\n",
            to_hex(&BOB)
        );
        let book = parse_book(&text);
        assert_eq!(book.links.len(), 1);
        assert_eq!(book.links[&ALICE].name, "Alice Schmidt");
        assert!(book.not_now.contains(&BOB));
    }

    /// 1448: a person met after the device began is asked about, one from
    /// the history it pulled is not; without a time or a beginning,
    /// nothing asks. A bad `since` line costs only itself.
    #[test]
    fn the_banner_asks_only_for_a_person_met_on_this_device() {
        let book = parse_book("since 1790000000\n");
        assert!(book.met_here(Some(1_790_000_000)));
        assert!(book.met_here(Some(1_790_000_500)));
        assert!(!book.met_here(Some(1_789_999_999)), "from the history");
        assert!(!book.met_here(None), "no time");
        assert!(!Book::default().met_here(Some(1)), "no beginning");
        let bad = parse_book(&format!("since soon\nnot-now {}\n", to_hex(&BOB)));
        assert_eq!(bad.since, None);
        assert!(bad.not_now.contains(&BOB));
    }

    #[test]
    fn a_photo_is_kept_and_dropped() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(photo(home.path(), &ALICE), None);
        keep_photo(home.path(), &ALICE, Some(b"\x89PNG")).expect("kept");
        let path = photo(home.path(), &ALICE).expect("a photo");
        assert_eq!(std::fs::read(&path).expect("read"), b"\x89PNG");
        keep_photo(home.path(), &ALICE, None).expect("dropped");
        assert_eq!(photo(home.path(), &ALICE), None);
        keep_photo(home.path(), &ALICE, None).expect("none to drop");
        assert_eq!(load(home.path()), Book::default());
    }
}
