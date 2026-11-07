//! Who a peer is on the screen: the name its announces carry, the reader's
//! own name for it, and the trust state that follows from naming
//! (`docs/src/concepts/loquax-mailbox.md`, "Options for naming" and "Trust
//! levels"; `loquax-keys-and-layout.md`, section 7, slice 5).
//!
//! # The display rule
//!
//! The petname if the reader set one, otherwise the name of the address
//! book contact the reader linked the peer to (on a phone, 1139), otherwise
//! the name the peer announced, and in each case the address's first four
//! hex digits after it
//! (`Alice ·4f2a`): the hash is shortened, never suppressed, so "that is not
//! the Alice I know" stays possible at a glance. A peer that announced no
//! name is its address, `<9c31e0a2…>`, as before names arrived.
//!
//! # Names off the wire
//!
//! A name is arbitrary bytes, signed by its holder and claimed by nobody
//! else, so it is sanitised before it is drawn ([`sanitise`]) and never
//! trusted to be unique. Two marks follow from that:
//!
//! - **Collision**: a peer whose shown name looks like another peer's (the
//!   same [`skeleton`]: case, full-width forms and the common Cyrillic,
//!   Greek and digit look-alikes folded) carries [`COLLISION`] after its
//!   hash, both of them, and the header says whose name it shares.
//! - **Name change**: a peer that announces a different name than it did
//!   before in this session; the screen says so once in the conversation
//!   ([`Heard::Changed`]).
//!
//! Neither is a trust state. The pages rejected a derived "suspicious"
//! state: shared names and renames are normal mesh life, and a state that
//! cries wolf on them trains the reader to ignore it.
//!
//! # Trust
//!
//! Three states, `known` (the reader named the peer, or linked it to a
//! contact of the phone's address book), `unknown` and
//! `blocked`, from the contacts file ([`Contacts`]): `${LNMSG_HOME}/contacts`,
//! one `name <address> <petname>` or `block <address>` a line. The palette's
//! `rename`, `block` and `unblock` write it ([`edit_contacts`]): only the
//! lines about that peer change, every comment and every other line stays
//! as it was, and the file is replaced through a rename ([`save_contact`]).
//!
//! A message from a blocked address is received, proven and stored as any
//! other, so its sender sees it delivered and nothing on the wire tells; the
//! screen leaves it out of the list, the unread count, the toasts and the
//! map (`crate::ui`), and the palette's `blocked` lists those
//! conversations.
//!
//! A name heard is kept in the history store (`crate::store`) and named
//! again after a restart ([`Names::remember`]).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_lxmf::announce::DeliveryAnnounce;
use leviculum_ui::delivery::{Mark, DELIVERY_GLYPHS};
use leviculum_ui::text::{fit_width, truncate_to_cols};

use crate::address::{self, to_hex};

/// The widest a name is drawn, in terminal cells; a longer one is cut with
/// `…`. The pages ask for a cap and name no number: 32 cells keeps a whole
/// first and last name and leaves the short hash and the time room on a
/// list row at 120 columns.
pub const NAME_CELLS: usize = 32;

/// Hex digits of the address after a name.
pub const HASH_DIGITS: usize = 4;

/// What a name shares with another peer's, after the hash.
pub const COLLISION: char = '≈';

/// What a glyph the screen reserves becomes inside a name.
pub const REPLACEMENT: char = '?';

/// The contacts file's name under the loquax home directory.
pub const CONTACTS_FILE: &str = "contacts";

/// The display name in an announce's app data, as the reference reads it
/// (`LXMF.display_name_from_app_data`, `reference/LXMF/LXMF/LXMF.py:151-172`):
/// the first element of the msgpack list since LXMF 0.5 (`[name, stamp
/// cost, ...]`), or the whole app data in the original bare-bytes form;
/// UTF-8, NUL removed, whitespace trimmed. `None` for no name, an empty or
/// malformed app data, and bytes that are not UTF-8, where the reference
/// logs and returns `None` too. The router decodes the same way before it
/// says `PeerAnnounced` ([`DeliveryAnnounce::decode`]).
pub fn from_app_data(app_data: &[u8]) -> Option<String> {
    DeliveryAnnounce::decode(app_data).ok()?.display_name()
}

/// A name as it may be drawn: `None` when nothing visible is left.
///
/// - control characters, bidi embeddings, overrides and isolates, zero-width
///   and other invisible code points (Unicode's default-ignorables, the
///   variation selectors and tags among them) are removed;
/// - every run of white space, the exotic spaces included, is one space,
///   and none leads or trails;
/// - a glyph the screen draws as a mark of its own (the delivery marks, the
///   spinner's braille, check marks, the `·` before a hash and its
///   look-alikes, `…`, the search's `«` `»`, the collision mark, box
///   drawing and block elements) is [`REPLACEMENT`], so a name cannot draw
///   a delivery state, a hash suffix or a separator line;
/// - the width is capped at [`NAME_CELLS`] cells, a wide character counting
///   two.
pub fn sanitise(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if invisible(ch) {
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(match reserved(ch) {
            true => REPLACEMENT,
            false => ch,
        });
    }
    match out.is_empty() {
        true => None,
        false => Some(truncate_to_cols(&out, NAME_CELLS)),
    }
}

/// Removed from a name: draws nothing, or moves what is drawn around it.
fn invisible(ch: char) -> bool {
    ch.is_control()
        || matches!(
            u32::from(ch),
            0x00AD // soft hyphen
            | 0x034F // combining grapheme joiner
            | 0x061C // Arabic letter mark
            | 0x115F..=0x1160 // Hangul fillers
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F // Mongolian selectors and vowel separator
            | 0x200B..=0x200F // zero-width space, joiners, LRM, RLM
            | 0x202A..=0x202E // bidi embeddings and overrides
            | 0x2060..=0x206F // word joiner, invisible operators, isolates
            | 0x3164 // Hangul filler
            | 0xFE00..=0xFE0F // variation selectors
            | 0xFEFF // zero-width no-break space
            | 0xFFA0 // half-width Hangul filler
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF // tags and supplementary variation selectors
        )
}

/// A glyph the screen uses for a meaning of its own.
fn reserved(ch: char) -> bool {
    let mark = DELIVERY_GLYPHS.iter().any(|row| match row.mark {
        // The column's padding and the ASCII `!` stay a name's own.
        Mark::Glyph(glyph) => glyph.chars().any(|g| g == ch && !g.is_ascii()),
        Mark::Spinner => false,
    });
    mark || ch == COLLISION
        || matches!(
            u32::from(ch),
            0x00AB | 0x00BB // « » the search's marks
            | 0x00B7 | 0x0387 | 0x2022 | 0x2027 | 0x2219 | 0x22C5 | 0x30FB | 0xFF65 // · and its look-alikes
            | 0x2026 // …
            | 0x2500..=0x259F // box drawing and block elements: borders, rules, the bar
            | 0x2611..=0x2612 // ☑ ☒
            | 0x2705 // ✅
            | 0x2713..=0x2718 // ✓ ✔ ✕ ✖ ✗ ✘
            | 0x274C | 0x274E // ❌ ❎
            | 0x2800..=0x28FF // braille, the spinner's
        )
}

/// What a name looks like, for telling two names apart: lower case, the
/// full-width forms as ASCII, and the Cyrillic, Greek and digit letters that
/// draw like a Latin one folded onto it. Two names with one skeleton are a
/// collision. The pages ask that a name not render as another contact's;
/// Unicode's full confusables table is a dependency this crate does not
/// carry, so this folds the look-alikes an impersonation reaches for first.
pub fn skeleton(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        let ch = match u32::from(ch) {
            code @ 0xFF01..=0xFF5E => char::from_u32(code - 0xFEE0).unwrap_or(ch),
            _ => ch,
        };
        for lower in ch.to_lowercase() {
            out.push(fold(lower));
        }
    }
    out
}

/// One lower-case letter onto the Latin one it draws like.
fn fold(ch: char) -> char {
    match ch {
        'а' | 'α' => 'a',
        'в' | 'β' => 'b',
        'с' | 'ϲ' => 'c',
        'ԁ' => 'd',
        'е' | 'ε' => 'e',
        'ɡ' => 'g',
        'һ' | 'н' | 'η' => 'h',
        // Capital I draws as small l in most terminal fonts, and folds to
        // i before it gets here, so i, l and their look-alikes are one
        // letter: "Ian" and "lan" collide, which a mark may afford.
        'i' | 'і' | 'ι' | 'ı' | 'l' | 'ӏ' | 'ℓ' | '1' | '|' => 'l',
        'ј' => 'j',
        'к' | 'κ' => 'k',
        'м' | 'μ' => 'm',
        'о' | 'ο' | '0' => 'o',
        'р' | 'ρ' => 'p',
        'ԛ' => 'q',
        'ѕ' => 's',
        'т' | 'τ' => 't',
        'υ' => 'u',
        'ν' => 'v',
        'ԝ' => 'w',
        'х' | 'χ' => 'x',
        'у' | 'γ' => 'y',
        'ζ' => 'z',
        other => other,
    }
}

/// The address's first hex digits, as they follow a name: `·4f2a`.
pub fn hash_suffix(peer: &[u8; 16]) -> String {
    let mut hex = to_hex(&peer[..HASH_DIGITS / 2]);
    hex.insert(0, '·');
    hex
}

/// A peer that announced no name and has no petname: its address.
pub fn address_form(peer: &[u8; 16]) -> String {
    format!("<{}…>", to_hex(&peer[..4]))
}

/// The three trust states, said in words (never a glyph alone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The reader named this peer.
    Known,
    /// The reader has not.
    Unknown,
    /// The reader blocked it.
    Blocked,
}

impl Trust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Unknown => "unknown",
            Self::Blocked => "blocked",
        }
    }
}

/// The reader's own word on peers: petnames and blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contacts {
    /// Sanitised like an announced name: the file is text anyone may have
    /// written.
    pub petnames: BTreeMap<[u8; 16], String>,
    pub blocked: BTreeSet<[u8; 16]>,
}

impl Contacts {
    /// The reader's word on `peer`.
    pub fn of(&self, peer: &[u8; 16]) -> Contact {
        Contact {
            peer: *peer,
            petname: self.petnames.get(peer).cloned(),
            blocked: self.blocked.contains(peer),
        }
    }

    /// Take `contact` as the reader's word on its peer.
    pub fn set(&mut self, contact: &Contact) {
        match &contact.petname {
            Some(name) => self.petnames.insert(contact.peer, name.clone()),
            None => self.petnames.remove(&contact.peer),
        };
        match contact.blocked {
            true => self.blocked.insert(contact.peer),
            false => self.blocked.remove(&contact.peer),
        };
    }
}

/// Everything the contacts file says about one peer: what the palette's
/// `rename`, `block` and `unblock` change, whole, so writing it twice is
/// writing it once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    pub peer: [u8; 16],
    /// Sanitised ([`sanitise`]); `None` for none.
    pub petname: Option<String>,
    pub blocked: bool,
}

/// The contacts text with `contact`'s lines made to say it: its first `name`
/// and `block` line replaced in place or removed, a later duplicate of
/// either removed, a missing one appended. Every other line, comments,
/// blank lines and lines this version does not read among them, comes back
/// byte for byte, so the file stays the reader's.
pub fn edit_contacts(text: &str, contact: &Contact) -> String {
    let hex = to_hex(&contact.peer);
    let name_line = contact
        .petname
        .as_ref()
        .map(|pet| format!("name {hex} {pet}"));
    let block_line = contact.blocked.then(|| format!("block {hex}"));
    let (mut named, mut blocked) = (false, false);
    let mut out = String::new();
    for raw in text.split_inclusive('\n') {
        let line = raw.trim();
        let mut words = line.split_whitespace();
        let verb = words.next();
        let about = words.next().and_then(|w| address::parse(w).ok()) == Some(contact.peer);
        let (wanted, done) = match verb {
            Some("name") if about => (&name_line, &mut named),
            Some("block") if about => (&block_line, &mut blocked),
            _ => {
                out.push_str(raw);
                continue;
            }
        };
        if let Some(wanted) = wanted.as_ref().filter(|_| !*done) {
            out.push_str(wanted);
            out.push('\n');
            *done = true;
        }
    }
    for (wanted, done) in [(&name_line, named), (&block_line, blocked)] {
        if let Some(wanted) = wanted.as_ref().filter(|_| !done) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(wanted);
            out.push('\n');
        }
    }
    out
}

/// Why the contacts text could not be used, with the line (1-based).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactsError {
    pub line: usize,
    pub detail: String,
}

impl core::fmt::Display for ContactsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "line {}: {}", self.line, self.detail)
    }
}

impl core::error::Error for ContactsError {}

/// Read the contacts file's text:
///
/// ```text
/// # the reader's names for peers
/// name 4f2a91c03be8d7e15a60f2b44c9d0e71 Alice from the field team
/// block 9c31e0a2b7d04411aa00bb11cc22dd33
/// ```
///
/// A line that is not one of the two is an error naming the line, as a
/// wrong value in `config` is: a typo in an address would otherwise leave a
/// peer quietly `unknown`.
pub fn parse_contacts(text: &str) -> Result<Contacts, ContactsError> {
    let mut contacts = Contacts::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |detail: String| ContactsError {
            line: index + 1,
            detail,
        };
        let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim_start();
        let (hex, name) = rest
            .split_once(char::is_whitespace)
            .map_or((rest, ""), |(hex, name)| (hex, name.trim()));
        let peer = address::parse(hex).map_err(|e| bad(format!("{verb}: {e}")))?;
        match verb {
            "name" => match sanitise(name) {
                Some(name) => {
                    contacts.petnames.insert(peer, name);
                }
                None => return Err(bad("name: no name after the address".to_string())),
            },
            "block" if name.is_empty() => {
                contacts.blocked.insert(peer);
            }
            "block" => return Err(bad("block: nothing may follow the address".to_string())),
            other => {
                return Err(bad(format!("{other:?} is not name or block")));
            }
        }
    }
    Ok(contacts)
}

/// Why `home/contacts` could not be used, with the file named.
#[cfg(feature = "std")]
#[derive(Debug)]
pub enum LoadError {
    Io(std::path::PathBuf, std::io::Error),
    Bad(std::path::PathBuf, ContactsError),
}

#[cfg(feature = "std")]
impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Bad(path, error) => {
                write!(f, "{}:{}: {}", path.display(), error.line, error.detail)
            }
        }
    }
}

#[cfg(feature = "std")]
impl core::error::Error for LoadError {}

/// Load `home/contacts`. A missing file is no contacts.
#[cfg(feature = "std")]
pub fn load_contacts(home: &std::path::Path) -> Result<Contacts, LoadError> {
    let path = home.join(CONTACTS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Contacts::default());
        }
        Err(error) => return Err(LoadError::Io(path, error)),
    };
    parse_contacts(&text).map_err(|error| LoadError::Bad(path, error))
}

/// Make `home/contacts` say `contact` ([`edit_contacts`]), through a rename
/// so a crash leaves the old file whole. A missing file is an empty one.
#[cfg(feature = "std")]
pub fn save_contact(home: &std::path::Path, contact: &Contact) -> std::io::Result<()> {
    let path = home.join(CONTACTS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, edit_contacts(&text, contact))?;
    std::fs::rename(&tmp, &path)
}

/// What an announce changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// The first name this session heard from the peer.
    New,
    /// The name it announced before.
    Same,
    /// A different name than before: the screen says so once.
    Changed { was: String, now: String },
    /// An announce with no name, or none left after sanitising: the name
    /// heard before, if any, stays.
    Nameless,
}

/// A peer linked to a contact of the phone's address book (1139): the
/// contact's name as the link copied it, sanitised, and which contact it
/// is, so two addresses of one person are told apart from two people who
/// share a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    pub name: String,
    pub contact: String,
}

/// Every name this session heard, and the reader's contacts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    /// The sanitised name each peer last announced.
    announced: BTreeMap<[u8; 16], String>,
    pub contacts: Contacts,
    /// The address book's names of the linked peers: the reader's word
    /// outside loquax, below a petname, above an announce.
    book: BTreeMap<[u8; 16], Linked>,
}

impl Names {
    pub fn new(contacts: Contacts) -> Self {
        Self {
            announced: BTreeMap::new(),
            contacts,
            book: BTreeMap::new(),
        }
    }

    /// Link `peer` to the address book's `contact`, named `name` there; a
    /// name that sanitises to nothing unlinks.
    pub fn link(&mut self, peer: [u8; 16], name: &str, contact: &str) {
        match sanitise(name) {
            Some(name) => {
                self.book.insert(
                    peer,
                    Linked {
                        name,
                        contact: contact.to_string(),
                    },
                );
            }
            None => self.unlink(&peer),
        }
    }

    pub fn unlink(&mut self, peer: &[u8; 16]) {
        self.book.remove(peer);
    }

    /// The address book's name for the peer, when it is linked.
    pub fn book_name(&self, peer: &[u8; 16]) -> Option<&str> {
        self.book.get(peer).map(|linked| linked.name.as_str())
    }

    /// The other addresses linked to the peer's contact: the same person.
    pub fn same_contact(&self, peer: &[u8; 16]) -> Vec<[u8; 16]> {
        let Some(mine) = self.book.get(peer) else {
            return Vec::new();
        };
        self.book
            .iter()
            .filter(|(other, linked)| *other != peer && linked.contact == mine.contact)
            .map(|(other, _)| *other)
            .collect()
    }

    /// Note an announce from `peer` carrying `name`, as the reference
    /// decodes it ([`from_app_data`]).
    pub fn heard(&mut self, peer: [u8; 16], name: Option<&str>) -> Heard {
        let Some(name) = name.and_then(sanitise) else {
            return Heard::Nameless;
        };
        match self.announced.insert(peer, name.clone()) {
            None => Heard::New,
            Some(was) if was == name => Heard::Same,
            Some(was) => Heard::Changed { was, now: name },
        }
    }

    /// A name the store kept from an earlier run: the peer is named by it
    /// until it announces again, and a different name then is a change.
    pub fn remember(&mut self, peer: [u8; 16], name: &str) {
        if let Some(name) = sanitise(name) {
            self.announced.insert(peer, name);
        }
    }

    /// How many peers announced a name this session.
    pub fn heard_count(&self) -> usize {
        self.announced.len()
    }

    /// Everything a screen names a peer by, folded into `hash` (1490):
    /// part of the model's look fingerprint, so a name heard rebuilds the
    /// rows that show it.
    pub(crate) fn fingerprint(&self, hash: &mut crate::ui::Fnv) {
        for (peer, name) in &self.announced {
            hash.bytes(peer);
            hash.bytes(name.as_bytes());
        }
        for (peer, linked) in &self.book {
            hash.bytes(peer);
            hash.bytes(linked.name.as_bytes());
        }
        for (peer, petname) in &self.contacts.petnames {
            hash.bytes(peer);
            hash.bytes(petname.as_bytes());
        }
        for peer in &self.contacts.blocked {
            hash.bytes(peer);
        }
    }

    /// The name the peer last announced, sanitised.
    pub fn announced(&self, peer: &[u8; 16]) -> Option<&str> {
        self.announced.get(peer).map(String::as_str)
    }

    /// The reader's name for the peer.
    pub fn petname(&self, peer: &[u8; 16]) -> Option<&str> {
        self.contacts.petnames.get(peer).map(String::as_str)
    }

    /// The display rule's name: the petname, else the address book's, else
    /// the announced one.
    pub fn shown(&self, peer: &[u8; 16]) -> Option<&str> {
        self.petname(peer)
            .or_else(|| self.book_name(peer))
            .or_else(|| self.announced(peer))
    }

    pub fn trust(&self, peer: &[u8; 16]) -> Trust {
        if self.contacts.blocked.contains(peer) {
            Trust::Blocked
        } else if self.contacts.petnames.contains_key(peer) || self.book.contains_key(peer) {
            Trust::Known
        } else {
            Trust::Unknown
        }
    }

    /// Every peer with a name to show.
    fn named(&self) -> impl Iterator<Item = &[u8; 16]> {
        let heard = self.announced.keys();
        let named = self
            .contacts
            .petnames
            .keys()
            .filter(|peer| !self.announced.contains_key(*peer));
        let linked = self.book.keys().filter(|peer| {
            !self.announced.contains_key(*peer) && !self.contacts.petnames.contains_key(*peer)
        });
        heard.chain(named).chain(linked)
    }

    /// The other peers whose shown name looks like this one's, by address.
    /// Two addresses linked to one contact are one person, the reader said
    /// so: no collision.
    pub fn collisions(&self, peer: &[u8; 16]) -> Vec<[u8; 16]> {
        let Some(mine) = self.shown(peer).map(skeleton) else {
            return Vec::new();
        };
        let same = self.same_contact(peer);
        self.named()
            .filter(|other| *other != peer && !same.contains(other))
            .filter(|other| self.shown(other).map(skeleton).as_deref() == Some(&mine))
            .copied()
            .collect()
    }

    /// The peer as every place on the screen names it: `Alice ·4f2a`,
    /// `Alice ·4f2a ≈` when another peer's name looks the same, or the
    /// address `<9c31e0a2…>` while it has no name.
    pub fn label(&self, peer: &[u8; 16]) -> String {
        self.label_in(peer, usize::MAX)
    }

    /// [`Self::label`] in at most `cols` cells: the name is cut first, so
    /// the hash and the mark stay.
    pub fn label_in(&self, peer: &[u8; 16], cols: usize) -> String {
        let Some(name) = self.shown(peer) else {
            return truncate_to_cols(&address_form(peer), cols);
        };
        let mut tail = format!(" {}", hash_suffix(peer));
        if !self.collisions(peer).is_empty() {
            tail.push(' ');
            tail.push(COLLISION);
        }
        let room = cols.saturating_sub(fit_width(&tail));
        if room == 0 {
            return truncate_to_cols(&tail[1..], cols);
        }
        format!("{}{tail}", truncate_to_cols(name, room))
    }

    /// The header's words after the trust state: whose name this one
    /// shares, the contact's other addresses, and the announced name beside
    /// a petname or an address book name that differs.
    pub fn header_notes(&self, peer: &[u8; 16]) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(other) = self.collisions(peer).first() {
            notes.push(format!("same name as {}", hash_suffix(other)));
        }
        let others = self.same_contact(peer);
        if !others.is_empty() {
            let hashes: Vec<String> = others.iter().map(hash_suffix).collect();
            notes.push(format!("also {}", hashes.join(" ")));
        }
        let own = self.petname(peer).or_else(|| self.book_name(peer));
        if let (Some(own), Some(announced)) = (own, self.announced(peer)) {
            if own != announced {
                notes.push(format!("announces {announced}"));
            }
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::announce::delivery;

    const ALICE: [u8; 16] = [0x4f, 0x2a, 0x91, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    const MALLORY: [u8; 16] = [0xb7, 0xd0, 0x11, 0x22, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
    const CAROL: [u8; 16] = [0x17, 0xee, 0x33, 0x44, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3];

    /// The positive control: a clean name, accents and CJK included, passes
    /// untouched, so every removal below is the rule's and not a blanket one.
    #[test]
    fn a_clean_name_passes_untouched() {
        for clean in [
            "Alice",
            "Jörg Müller",
            "李小龙",
            "lew@schneckenschreck",
            "O'Brien (field)",
        ] {
            assert_eq!(sanitise(clean).as_deref(), Some(clean), "{clean}");
        }
    }

    #[test]
    fn control_characters_are_removed() {
        assert_eq!(
            sanitise("Al\u{7}i\u{1b}[31mce\u{7f}\u{9b}").as_deref(),
            Some("Ali[31mce")
        );
        assert_eq!(sanitise("\u{0}\u{1}\u{1f}"), None);
    }

    #[test]
    fn line_breaks_and_exotic_spaces_are_one_space() {
        assert_eq!(
            sanitise("  Alice\n\tfrom\u{2003}\u{3000}the\u{a0}field  ").as_deref(),
            Some("Alice from the field")
        );
    }

    /// `evil` U+202E `gnp.exe` draws as `evilexe.png`: the override must go.
    #[test]
    fn bidi_overrides_embeddings_and_isolates_are_removed() {
        assert_eq!(
            sanitise("evil\u{202e}gnp.exe").as_deref(),
            Some("evilgnp.exe")
        );
        assert_eq!(
            sanitise("\u{2066}A\u{2067}l\u{2068}i\u{2069}c\u{202a}e\u{200f}\u{200e}\u{61c}")
                .as_deref(),
            Some("Alice")
        );
    }

    #[test]
    fn zero_width_and_invisible_code_points_are_removed() {
        assert_eq!(
            sanitise("A\u{200b}l\u{200c}i\u{200d}c\u{2060}e\u{feff}\u{ad}\u{fe0f}\u{e0041}")
                .as_deref(),
            Some("Alice")
        );
        // Nothing but invisibles is no name, not an empty one.
        assert_eq!(sanitise("\u{200b}\u{3164}\u{115f}"), None);
    }

    /// A name cannot draw a delivery state, a hash, a separator or a
    /// search mark.
    #[test]
    fn glyphs_the_screen_reserves_are_replaced() {
        assert_eq!(sanitise("Bob ✓").as_deref(), Some("Bob ?"));
        assert_eq!(sanitise("Bob ◷✗✔☑✅❌").as_deref(), Some("Bob ??????"));
        // The ladder's `!` and the glyphs it replaced (1232) are a name's.
        assert_eq!(sanitise("Hi! ◇⌂○").as_deref(), Some("Hi! ◇⌂○"));
        assert_eq!(sanitise("Alice ·4f2a").as_deref(), Some("Alice ?4f2a"));
        assert_eq!(sanitise("<9c31e0a2…>").as_deref(), Some("<9c31e0a2?>"));
        assert_eq!(sanitise("── Today ──").as_deref(), Some("?? Today ??"));
        assert_eq!(sanitise("«x» ⠹ ▌ ≈").as_deref(), Some("?x? ? ? ?"));
    }

    /// The cap is in cells: a wide character counts two, and the cut keeps
    /// the result inside the cap with its `…`.
    #[test]
    fn the_width_cap_counts_wide_characters_twice() {
        let ascii = "a".repeat(NAME_CELLS);
        assert_eq!(sanitise(&ascii).as_deref(), Some(ascii.as_str()));
        let long = "a".repeat(NAME_CELLS + 1);
        let cut = sanitise(&long).expect("a name");
        assert_eq!(fit_width(&cut), NAME_CELLS);
        assert!(cut.ends_with('…'), "{cut}");

        let wide = "龙".repeat(NAME_CELLS / 2);
        assert_eq!(sanitise(&wide).as_deref(), Some(wide.as_str()));
        let wider = "龙".repeat(NAME_CELLS / 2 + 1);
        let cut = sanitise(&wider).expect("a name");
        assert!(fit_width(&cut) <= NAME_CELLS, "{cut}");
        assert_eq!(
            cut.chars().filter(|&c| c == '龙').count(),
            NAME_CELLS / 2 - 1
        );
    }

    #[test]
    fn look_alikes_share_a_skeleton() {
        // Cyrillic А, Greek ο, capital I for l, full-width letters, a zero.
        for fake in ["Аlice", "AIice", "ＡＬＩＣＥ", "alice", "A1ice"] {
            assert_eq!(skeleton(fake), skeleton("Alice"), "{fake}");
        }
        assert_eq!(skeleton("Bοb"), skeleton("Bob"));
        assert_eq!(skeleton("B0b"), skeleton("Bob"));
        assert_ne!(skeleton("Alicia"), skeleton("Alice"));
    }

    /// The reference's two app data forms: msgpack since LXMF 0.5 (`[name,
    /// stamp cost, [compression]]`, and the older two-element list), and the
    /// original bare bytes.
    #[test]
    fn the_reference_app_data_forms_give_the_name() {
        assert_eq!(
            from_app_data(&delivery(Some(b"Alice"), Some(8))).as_deref(),
            Some("Alice")
        );
        assert_eq!(
            from_app_data(&delivery(Some(b"Alice"), None)).as_deref(),
            Some("Alice")
        );
        // [b"Alice", 8], the two-element form of LXMF 0.5 to 0.8.
        assert_eq!(
            from_app_data(&[0x92, 0xc4, 0x05, b'A', b'l', b'i', b'c', b'e', 0x08]).as_deref(),
            Some("Alice")
        );
        // The original format: the app data is the name.
        assert_eq!(from_app_data(b"Alice").as_deref(), Some("Alice"));
        // NUL removed and trimmed, as `display_name_from_app_data` does.
        assert_eq!(
            from_app_data(&delivery(Some(b" Al\0ice \0"), Some(8))).as_deref(),
            Some("Alice")
        );
        // A nil name is no name.
        assert_eq!(from_app_data(&delivery(None, Some(8))), None);
    }

    #[test]
    fn malformed_app_data_is_no_name_and_no_panic() {
        for bad in [
            &[][..],
            &[0x93][..],                                     // a list that ends at once
            &[0x91, 0xc4, 0x09, b'A'][..],                   // a name longer than the data
            &[0x92, 0xc4, 0x01, b'A', 0xcd, 0x01, 0x00][..], // a cost over 255
            &[0x91, 0xc4, 0x02, 0xff, 0xfe][..],             // not UTF-8
            &[0xff, 0xfe][..],                               // bare bytes, not UTF-8
            &[0x91, 0x01][..],                               // a number where the name is
        ] {
            assert_eq!(from_app_data(bad), None, "{bad:02x?}");
        }
    }

    #[test]
    fn the_label_is_the_name_and_the_short_hash() {
        let mut names = Names::default();
        assert_eq!(names.label(&ALICE), "<4f2a91c0…>");
        assert_eq!(names.heard(ALICE, Some("Alice")), Heard::New);
        assert_eq!(names.label(&ALICE), "Alice ·4f2a");
        // Cut, the name goes first and the hash stays.
        assert_eq!(names.label_in(&ALICE, 9), "Al… ·4f2a");
        assert_eq!(names.trust(&ALICE), Trust::Unknown);
    }

    #[test]
    fn two_peers_announcing_one_name_are_both_marked() {
        let mut names = Names::default();
        names.heard(ALICE, Some("Alice"));
        names.heard(CAROL, Some("Carol"));
        assert_eq!(names.label(&ALICE), "Alice ·4f2a");
        names.heard(MALLORY, Some("Аlice"));
        assert_eq!(names.label(&ALICE), "Alice ·4f2a ≈");
        assert_eq!(names.label(&MALLORY), "Аlice ·b7d0 ≈");
        assert_eq!(names.label(&CAROL), "Carol ·17ee");
        assert_eq!(names.header_notes(&ALICE), ["same name as ·b7d0"]);
        assert_eq!(names.header_notes(&MALLORY), ["same name as ·4f2a"]);
        // The mark goes when the clash does.
        names.heard(MALLORY, Some("Mallory"));
        assert_eq!(names.label(&ALICE), "Alice ·4f2a");
    }

    #[test]
    fn a_changed_name_is_reported_once() {
        let mut names = Names::default();
        assert_eq!(names.heard(ALICE, Some("Alice")), Heard::New);
        assert_eq!(names.heard(ALICE, Some("Alice")), Heard::Same);
        assert_eq!(
            names.heard(ALICE, Some("Ally")),
            Heard::Changed {
                was: "Alice".into(),
                now: "Ally".into()
            }
        );
        assert_eq!(names.heard(ALICE, Some("Ally")), Heard::Same);
        // An announce without a name keeps the one heard.
        assert_eq!(names.heard(ALICE, None), Heard::Nameless);
        assert_eq!(names.heard(ALICE, Some("\u{200b}")), Heard::Nameless);
        assert_eq!(names.label(&ALICE), "Ally ·4f2a");
        // A difference sanitising removes is no change.
        assert_eq!(names.heard(ALICE, Some("Al\u{200b}ly")), Heard::Same);
    }

    #[test]
    fn a_petname_wins_and_makes_the_peer_known() {
        let contacts = parse_contacts(&format!(
            "# mine\nname {} Mum\nblock {}\n",
            to_hex(&ALICE),
            to_hex(&MALLORY)
        ))
        .expect("well-formed");
        let mut names = Names::new(contacts);
        // Named before any announce.
        assert_eq!(names.label(&ALICE), "Mum ·4f2a");
        names.heard(ALICE, Some("Alice"));
        assert_eq!(names.label(&ALICE), "Mum ·4f2a");
        assert_eq!(names.trust(&ALICE), Trust::Known);
        assert_eq!(names.header_notes(&ALICE), ["announces Alice"]);
        assert_eq!(names.trust(&MALLORY), Trust::Blocked);
        assert_eq!(names.trust(&CAROL), Trust::Unknown);
        // A stranger announcing the petname collides with it.
        names.heard(CAROL, Some("mum"));
        assert_eq!(names.label(&CAROL), "mum ·17ee ≈");
        assert_eq!(names.label(&ALICE), "Mum ·4f2a ≈");
    }

    /// The name order of 1139: petname, then the address book's name, then
    /// the announced one; the short hash stays on each.
    #[test]
    fn a_petname_beats_the_address_book_which_beats_the_announce() {
        let mut names = Names::default();
        names.heard(ALICE, Some("Alice"));
        assert_eq!(names.label(&ALICE), "Alice ·4f2a");
        assert_eq!(names.trust(&ALICE), Trust::Unknown);
        names.link(ALICE, "Alice Schmidt", "lookup-1");
        assert_eq!(names.label(&ALICE), "Alice Schmidt ·4f2a");
        assert_eq!(names.trust(&ALICE), Trust::Known);
        assert_eq!(names.header_notes(&ALICE), ["announces Alice"]);
        names.contacts.set(&Contact {
            peer: ALICE,
            petname: Some("Mum".into()),
            blocked: false,
        });
        assert_eq!(names.label(&ALICE), "Mum ·4f2a");
        // The petname gone, the address book's name is back.
        names.contacts.set(&Contact {
            peer: ALICE,
            petname: None,
            blocked: false,
        });
        assert_eq!(names.label(&ALICE), "Alice Schmidt ·4f2a");
        names.unlink(&ALICE);
        assert_eq!(names.label(&ALICE), "Alice ·4f2a");
        assert_eq!(names.trust(&ALICE), Trust::Unknown);
        // Linked before any announce, the contact names it; a name that
        // sanitises to nothing is no link.
        names.link(CAROL, "Carol \u{202e}", "lookup-2");
        assert_eq!(names.label(&CAROL), "Carol ·17ee");
        names.link(CAROL, "\u{200b}", "lookup-2");
        assert_eq!(names.book_name(&CAROL), None);
    }

    /// Two addresses linked to one contact are one person: no collision
    /// mark, and the header names the other address. A stranger who
    /// announces the contact's name still collides with both.
    #[test]
    fn two_addresses_of_one_contact_do_not_collide() {
        let mut names = Names::default();
        names.link(ALICE, "Alice Schmidt", "lookup-1");
        names.link(CAROL, "Alice Schmidt", "lookup-1");
        assert_eq!(names.label(&ALICE), "Alice Schmidt ·4f2a");
        assert_eq!(names.label(&CAROL), "Alice Schmidt ·17ee");
        assert_eq!(names.header_notes(&ALICE), ["also ·17ee"]);
        names.heard(MALLORY, Some("alice schmidt"));
        assert_eq!(names.label(&ALICE), "Alice Schmidt ·4f2a ≈");
        assert_eq!(names.label(&MALLORY), "alice schmidt ·b7d0 ≈");
        // Two contacts of one name are two people: marked.
        names.unlink(&CAROL);
        names.link(CAROL, "Alice Schmidt", "lookup-9");
        assert_eq!(names.label(&CAROL), "Alice Schmidt ·17ee ≈");
    }

    #[test]
    fn a_bad_contacts_line_is_an_error_naming_it() {
        for (text, line) in [
            ("name nothex Alice\n", 1),
            ("\nname 4f2a91c0000000000000000000000001\n", 2),
            ("\n\nblock 4f2a91c0000000000000000000000001 extra\n", 3),
            ("call 4f2a91c0000000000000000000000001\n", 1),
        ] {
            let error = parse_contacts(text).expect_err(text);
            assert_eq!(error.line, line, "{text}: {error}");
        }
        // A petname is sanitised like an announced name.
        let contacts =
            parse_contacts("name 4f2a91c0000000000000000000000001 Mum ✓\u{202e}\n").expect("ok");
        assert_eq!(
            contacts.petnames.values().next().map(String::as_str),
            Some("Mum ?")
        );
    }

    #[test]
    fn a_missing_contacts_file_is_no_contacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            load_contacts(dir.path()).expect("no file"),
            Contacts::default()
        );
        std::fs::write(dir.path().join(CONTACTS_FILE), "frobnicate\n").expect("write");
        let text = load_contacts(dir.path()).expect_err("bad").to_string();
        assert!(text.contains("contacts:1: "), "{text}");
    }

    /// The palette's three writes: only the peer's own lines change, the
    /// comments, the blank line, the other peer's lines and their spelling
    /// come back byte for byte, and the text parses to what was set.
    #[test]
    fn an_edit_changes_the_peers_lines_and_keeps_every_other() {
        let (alice, mallory) = (to_hex(&ALICE), to_hex(&MALLORY));
        let text = format!(
            "# the reader's own names\n\nname {}   Mum  # kept as written\n\
             name {alice} Alice\n# blocked on 2026-10-01\nblock {mallory}\n",
            to_hex(&CAROL).to_uppercase(),
        );
        let named = Contact {
            peer: ALICE,
            petname: Some("Ally".into()),
            blocked: false,
        };
        let renamed = edit_contacts(&text, &named);
        assert_eq!(renamed, text.replace("Alice\n", "Ally\n"));
        assert_eq!(parse_contacts(&renamed).expect("parses").of(&ALICE), named);

        let blocked = Contact {
            peer: ALICE,
            petname: Some("Ally".into()),
            blocked: true,
        };
        let both = edit_contacts(&renamed, &blocked);
        assert_eq!(both, format!("{renamed}block {alice}\n"));
        // Writing the same word again changes nothing.
        assert_eq!(edit_contacts(&both, &blocked), both);

        let cleared = Contact {
            peer: ALICE,
            petname: None,
            blocked: true,
        };
        let unnamed = edit_contacts(&both, &cleared);
        assert_eq!(unnamed, both.replace(&format!("name {alice} Ally\n"), ""));
        let contacts = parse_contacts(&unnamed).expect("parses");
        assert_eq!(contacts.of(&ALICE), cleared);
        assert_eq!(
            contacts.petnames.get(&CAROL).map(String::as_str),
            Some("Mum # kept as written")
        );
        assert!(contacts.blocked.contains(&MALLORY));

        let unblocked = Contact {
            peer: MALLORY,
            petname: None,
            blocked: false,
        };
        let lifted = edit_contacts(&unnamed, &unblocked);
        assert_eq!(lifted, unnamed.replace(&format!("block {mallory}\n"), ""));
        assert!(lifted.contains("# blocked on 2026-10-01\n"), "{lifted}");
    }

    /// A duplicate line for the peer goes, the first stands for both; a text
    /// without a last newline gets one before what is appended, and an
    /// empty text is the new lines alone.
    #[test]
    fn an_edit_settles_duplicates_and_a_missing_newline() {
        let alice = to_hex(&ALICE);
        let text = format!("name {alice} One\nblock {alice}\nname {alice} Two\nblock {alice}");
        let contact = Contact {
            peer: ALICE,
            petname: Some("Three".into()),
            blocked: true,
        };
        assert_eq!(
            edit_contacts(&text, &contact),
            format!("name {alice} Three\nblock {alice}\n")
        );
        let comment = "# no newline at the end";
        assert_eq!(
            edit_contacts(comment, &contact),
            format!("{comment}\nname {alice} Three\nblock {alice}\n")
        );
        assert_eq!(
            edit_contacts(
                "",
                &Contact {
                    peer: ALICE,
                    petname: None,
                    blocked: true
                }
            ),
            format!("block {alice}\n")
        );
    }

    /// `save_contact` writes through a rename and keeps the comments; a
    /// missing file is made.
    #[test]
    fn a_saved_contact_round_trips_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contact = Contact {
            peer: ALICE,
            petname: Some("Mum".into()),
            blocked: false,
        };
        save_contact(dir.path(), &contact).expect("made");
        assert_eq!(
            load_contacts(dir.path()).expect("loads").of(&ALICE),
            contact
        );

        let path = dir.path().join(CONTACTS_FILE);
        let kept = format!("# mine\n{}", std::fs::read_to_string(&path).expect("read"));
        std::fs::write(&path, &kept).expect("write");
        let blocked = Contact {
            blocked: true,
            ..contact
        };
        save_contact(dir.path(), &blocked).expect("saved");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with(&kept), "{text}");
        assert_eq!(
            load_contacts(dir.path()).expect("loads").of(&ALICE),
            blocked
        );
        assert!(!dir.path().join("contacts.tmp").exists());
    }
}
