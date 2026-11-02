//! Finding a site by name, and pinning the key behind the name.
//!
//! A NomadNet node announces a display name, but nothing in Reticulum makes a
//! name unique or authoritative: anyone can announce "Marcel's Node". The
//! destination hash is the address. So a name typed into the address field is
//! a **search** over what this browser has seen, never a lookup: the announces
//! in the discovery registry ([`NomadNodeRegistry`], fed by
//! `discovery.rs::NomadNodeRegistry::observe` and `fetch.rs::Session::note_announce`)
//! and the sites already pinned. Every hit is shown as `name ·hash4`, the
//! spelling Loquax uses (`loquax/src/names.rs::hash_suffix`), so the short
//! hash is always part of what the reader recognises.
//!
//! A **pin** is the SSH `known_hosts` idea: the first visit to a destination
//! records `name -> full hash` ([`PinStore::note_visit`]). A later hit that
//! announces a pinned name under a different hash is marked
//! [`PinMark::Conflict`] and sorted below the pinned one; opening it takes an
//! explicit confirmation, after which the second hash is pinned beside the
//! first under the same name ([`PinStore::pin`]). Nothing is ever replaced
//! silently.
//!
//! The pins live at `${XDG_CONFIG_HOME:-~/.config}/lnomad/pins.toml` (see
//! [`default_path`]), TOML like the bookmarks and identify stores beside it:
//!
//! ```toml
//! [[pin]]
//! name = "Marcel Adingbatponder"
//! dest = "fa01..."
//! first_seen = 1761955200
//! last_seen = 1761958800
//! ```
//!
//! Loading tolerates a missing or corrupt file by starting empty; a row whose
//! `dest` is not a 32-hex hash is dropped on load.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

#[cfg(feature = "std")]
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::strings::{with, Arg, Lang, StrId, Text};
use crate::discovery::NomadNodeRegistry;

/// Hex digits of the short hash shown after a name (`name ·fa01`), the same
/// four Loquax shows.
pub const HASH_DIGITS: usize = 4;

/// Lowercase hex of a destination hash.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode a 32-hex destination hash, `None` for anything else.
fn from_hex(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The short hash as shown after a name: the first [`HASH_DIGITS`] hex digits.
pub fn hash4(dest: &[u8; 16]) -> String {
    to_hex(&dest[..HASH_DIGITS / 2])
}

/// A site as the result list names it: `name ·hash4`, or `·hash4` alone when
/// the site announced no name.
pub fn site_label(name: Option<&str>, dest: &[u8; 16]) -> String {
    match name {
        Some(name) => format!("{name} ·{}", hash4(dest)),
        None => format!("·{}", hash4(dest)),
    }
}

/// Whether two names are the same name for pinning: equal after trimming and
/// case folding, so `marcel` and `Marcel ` are one name.
pub fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// One pinned `name -> hash` pair with when it was first and last visited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    /// The name the site announced when it was pinned.
    pub name: String,
    /// The full destination hash as lowercase hex.
    pub dest: String,
    /// Unix seconds of the visit that pinned it.
    pub first_seen: u64,
    /// Unix seconds of the latest visit.
    pub last_seen: u64,
}

impl Pin {
    /// The destination hash, `None` when the stored hex is malformed (such a
    /// row never survives [`PinStore::load`]).
    pub fn dest_hash(&self) -> Option<[u8; 16]> {
        from_hex(&self.dest)
    }
}

/// What a visit did to the pins, so the caller knows whether to persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitOutcome {
    /// The destination was not pinned and is now, under its name.
    Pinned,
    /// The destination was already pinned; its `last_seen` moved.
    Touched,
    /// Nothing was pinned: the name is unknown, or it is pinned to another
    /// hash and the reader has not confirmed this one.
    Unchanged,
}

/// The pinned sites, in pinning order.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinStore {
    /// The pins. `#[serde(default)]` so an empty file loads as no pins.
    #[serde(default, rename = "pin")]
    items: Vec<Pin>,
}

impl PinStore {
    /// No pins.
    pub fn new() -> Self {
        Self::default()
    }

    /// The pins in pinning order.
    pub fn list(&self) -> &[Pin] {
        &self.items
    }

    /// The names `dest` is pinned under.
    pub fn names_for(&self, dest: &[u8; 16]) -> Vec<&str> {
        let hex = to_hex(dest);
        self.items
            .iter()
            .filter(|p| p.dest == hex)
            .map(|p| p.name.as_str())
            .collect()
    }

    /// The hashes `name` is pinned to.
    pub fn dests_for(&self, name: &str) -> Vec<[u8; 16]> {
        self.items
            .iter()
            .filter(|p| same_name(&p.name, name))
            .filter_map(Pin::dest_hash)
            .collect()
    }

    /// Whether `dest` is pinned under `name`.
    pub fn is_pinned(&self, name: &str, dest: &[u8; 16]) -> bool {
        self.dests_for(name).contains(dest)
    }

    /// Pin `dest` under `name`, the explicit form: used when the reader
    /// confirmed opening a site whose name is pinned to another hash. A pair
    /// already present only has its `last_seen` moved. Returns `true` when a
    /// new pair was added.
    pub fn pin(&mut self, name: &str, dest: &[u8; 16], now: u64) -> bool {
        let hex = to_hex(dest);
        if let Some(pin) = self
            .items
            .iter_mut()
            .find(|p| p.dest == hex && same_name(&p.name, name))
        {
            pin.last_seen = now;
            return false;
        }
        self.items.push(Pin {
            name: name.trim().to_string(),
            dest: hex,
            first_seen: now,
            last_seen: now,
        });
        true
    }

    /// Record a visit to `dest`, whose announced name is `name`: the implicit
    /// pin. A destination already pinned (under any name) is touched; an
    /// unpinned one is pinned under its name unless that name is pinned to a
    /// different hash, which only an explicit [`pin`](Self::pin) after the
    /// reader confirmed may add to.
    pub fn note_visit(&mut self, name: Option<&str>, dest: &[u8; 16], now: u64) -> VisitOutcome {
        let hex = to_hex(dest);
        let mut touched = false;
        for pin in self.items.iter_mut().filter(|p| p.dest == hex) {
            pin.last_seen = now;
            touched = true;
        }
        if touched {
            return VisitOutcome::Touched;
        }
        let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
            return VisitOutcome::Unchanged;
        };
        if !self.dests_for(name).is_empty() {
            return VisitOutcome::Unchanged;
        }
        self.pin(name, dest, now);
        VisitOutcome::Pinned
    }

    /// Load the pins from `path`, tolerating a missing or corrupt file by
    /// starting empty, and dropping any row whose hash is not 32 hex digits.
    #[cfg(feature = "std")]
    pub fn load(path: &Path) -> Self {
        let mut store: Self = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        store.items.retain(|p| p.dest_hash().is_some());
        for pin in &mut store.items {
            pin.dest.make_ascii_lowercase();
        }
        store
    }

    /// Write the pins to `path` as TOML, creating parent directories.
    #[cfg(feature = "std")]
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

/// The default pin-store path: `${XDG_CONFIG_HOME:-~/.config}/lnomad/
/// pins.toml`. `None` when no config dir can be resolved; pins then last for
/// the session only.
#[cfg(feature = "std")]
pub fn default_path() -> Option<PathBuf> {
    Some(crate::identity::app_config_dir()?.join("pins.toml"))
}

/// How a search hit stands against the pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinMark {
    /// Pinned under the name it announces.
    Pinned,
    /// Not pinned, and its name is not pinned to anything else.
    Unpinned,
    /// Its name is pinned, but to a different hash: possibly someone else
    /// using the same name. Opening it asks first.
    Conflict,
    /// Its hash is pinned, under another name than it now announces.
    PinnedAs(String),
}

/// One row of a name search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The destination hash.
    pub dest: [u8; 16],
    /// The name the site announced, or the pinned name for a pinned site that
    /// has not announced in this session.
    pub name: Option<String>,
    /// Hops from the most recent announce, if one was seen this session.
    pub hops: Option<u32>,
    /// Unix seconds the site was last heard from (announce) or visited (pin).
    pub last_seen: u64,
    /// How it stands against the pins.
    pub mark: PinMark,
}

impl SearchHit {
    /// The row's label, `name ·hash4`.
    pub fn label(&self) -> String {
        site_label(self.name.as_deref(), &self.dest)
    }
}

/// The pin mark for a site announcing `name` at `dest`.
fn mark_for(pins: &PinStore, name: Option<&str>, dest: &[u8; 16]) -> PinMark {
    let pinned_names = pins.names_for(dest);
    match name {
        Some(name) if pinned_names.iter().any(|n| same_name(n, name)) => PinMark::Pinned,
        _ if !pinned_names.is_empty() => PinMark::PinnedAs(pinned_names[0].to_string()),
        Some(name) if !pins.dests_for(name).is_empty() => PinMark::Conflict,
        _ => PinMark::Unpinned,
    }
}

/// Whether a site matches `query` (already trimmed and lowercased): a
/// substring of its `name ·hash4` label, so the name, the short hash and the
/// label pasted whole all find it, or a prefix of its full hash.
fn matches(name: Option<&str>, dest: &[u8; 16], query: &str) -> bool {
    site_label(name, dest).to_lowercase().contains(query) || to_hex(dest).starts_with(query)
}

/// Search the announces seen this session and the pinned sites for `query`.
///
/// Matching is case insensitive ([`matches`]). A site announcing in this
/// session is listed with its announced name; a pinned site that has not
/// announced yet is listed under its pinned name with unknown hops. Rows sort
/// with pinned sites first and conflicting ones last, then by name, then most
/// recently heard first, so a site wearing a pinned name under another key is
/// always below the one that was visited.
pub fn search(registry: &NomadNodeRegistry, pins: &PinStore, query: &str) -> Vec<SearchHit> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<SearchHit> = registry
        .nodes()
        .into_iter()
        .filter(|n| matches(n.name.as_deref(), &n.dest_hash, &query))
        .map(|n| SearchHit {
            dest: n.dest_hash,
            name: n.name.clone(),
            hops: n.hops,
            last_seen: n.last_seen,
            mark: mark_for(pins, n.name.as_deref(), &n.dest_hash),
        })
        .collect();
    for pin in pins.list() {
        let Some(dest) = pin.dest_hash() else {
            continue;
        };
        if registry.get_by_hash(&dest).is_some() || hits.iter().any(|h| h.dest == dest) {
            continue;
        }
        if matches(Some(&pin.name), &dest, &query) {
            hits.push(SearchHit {
                dest,
                name: Some(pin.name.clone()),
                hops: None,
                last_seen: pin.last_seen,
                mark: PinMark::Pinned,
            });
        }
    }
    let rank = |m: &PinMark| match m {
        PinMark::Pinned | PinMark::PinnedAs(_) => 0,
        PinMark::Unpinned => 1,
        PinMark::Conflict => 2,
    };
    hits.sort_by(|a, b| {
        rank(&a.mark)
            .cmp(&rank(&b.mark))
            .then_with(|| {
                let an = a.name.as_deref().unwrap_or("").to_lowercase();
                let bn = b.name.as_deref().unwrap_or("").to_lowercase();
                an.cmp(&bn)
            })
            .then_with(|| b.last_seen.cmp(&a.last_seen))
    });
    hits
}

/// The hit to open without showing the list: the only hit whose name is
/// exactly `query` (case folded), and only when it is not a conflict. Two
/// sites with that name, or one that would need a confirmation, show the list.
pub fn direct_hit<'a>(hits: &'a [SearchHit], query: &str) -> Option<&'a SearchHit> {
    let mut exact = hits
        .iter()
        .filter(|h| h.name.as_deref().is_some_and(|n| same_name(n, query)));
    let only = exact.next()?;
    if exact.next().is_some() || only.mark == PinMark::Conflict {
        return None;
    }
    Some(only)
}

/// How long ago `then` was, at `now`, in the result list's words.
pub fn ago(now: u64, then: u64) -> String {
    ago_text(now, then).resolve(Lang::En)
}

/// [`ago`] in the catalogue's words.
pub fn ago_text(now: u64, then: u64) -> Text {
    let secs = now.saturating_sub(then);
    let n = |id: StrId, unit: u64| Text::Key(id, vec![Arg::Int((secs / unit) as i64)]);
    match secs {
        0..=59 => Text::key(StrId::JustNow),
        60..=3599 => n(StrId::MinutesAgo, 60),
        3600..=86_399 => n(StrId::HoursAgo, 3600),
        _ => n(StrId::DaysAgo, 86_400),
    }
}

/// The keys a name is pinned to, as `·hash4` words.
fn pinned_to(pins: &PinStore, name: &str) -> String {
    let pinned: Vec<String> = pins
        .dests_for(name)
        .iter()
        .map(|d| format!("·{}", hash4(d)))
        .collect();
    pinned.join(" ")
}

/// One result-list row: `name ·hash4`, hops, last seen, and the pin mark.
pub fn result_row(hit: &SearchHit, pins: &PinStore, now: u64) -> String {
    result_text(hit, pins, now).resolve(Lang::En)
}

/// [`result_row`] in the catalogue's words.
pub fn result_text(hit: &SearchHit, pins: &PinStore, now: u64) -> Text {
    let hops = match hit.hops {
        Some(1) => Text::key(StrId::OneHop),
        Some(h) => Text::Key(StrId::Hops, vec![Arg::Int(h.into())]),
        None => Text::key(StrId::HopsNone),
    };
    let mark = match &hit.mark {
        PinMark::Pinned => Text::key(StrId::MarkPinned),
        PinMark::Unpinned => Text::Raw(String::new()),
        PinMark::Conflict => with(
            StrId::MarkConflict,
            pinned_to(pins, hit.name.as_deref().unwrap_or("")),
        ),
        PinMark::PinnedAs(name) => with(StrId::MarkPinnedAs, name.clone()),
    };
    Text::Key(
        StrId::NameRow,
        vec![
            Arg::Str(hit.label()),
            Arg::Text(Box::new(hops)),
            Arg::Text(Box::new(ago_text(now, hit.last_seen))),
            Arg::Text(Box::new(mark)),
        ],
    )
}

/// The question asked before opening a hit whose name is pinned to another
/// hash: which key the name is pinned to, which one this is, and what a yes
/// does.
pub fn conflict_prompt(hit: &SearchHit, pins: &PinStore) -> String {
    conflict_text(hit, pins).resolve(Lang::En)
}

/// [`conflict_prompt`] in the catalogue's words.
pub fn conflict_text(hit: &SearchHit, pins: &PinStore) -> Text {
    let name = hit.name.as_deref().unwrap_or("");
    Text::Key(
        StrId::NameConflict,
        vec![
            Arg::Str(name.to_string()),
            Arg::Str(pinned_to(pins, name)),
            Arg::Str(hash4(&hit.dest)),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARCEL: [u8; 16] = [
        0xfa, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
        0xcd,
    ];
    const IMPOSTOR: [u8; 16] = [0x9c; 16];
    const OTHER: [u8; 16] = [0x17; 16];

    fn registry(nodes: &[([u8; 16], &str, u32, u64)]) -> NomadNodeRegistry {
        let mut reg = NomadNodeRegistry::new();
        for (dest, name, hops, seen) in nodes {
            reg.upsert(*dest, Some(name.to_string()), Some(*hops), *seen);
        }
        reg
    }

    #[test]
    fn search_finds_a_name_substring_case_folded() {
        let reg = registry(&[
            (MARCEL, "Marcel Adingbatponder", 2, 100),
            (OTHER, "Solar Nord", 1, 90),
        ]);
        let pins = PinStore::new();
        for query in ["adingbat", "MARCEL", "marcel adingbatponder", "  Marcel "] {
            let hits = search(&reg, &pins, query);
            assert_eq!(hits.len(), 1, "{query:?}");
            assert_eq!(hits[0].dest, MARCEL);
            assert_eq!(hits[0].label(), "Marcel Adingbatponder ·fa01");
            assert_eq!(hits[0].hops, Some(2));
        }
        // The negative control: a query matching neither site finds nothing.
        assert!(search(&reg, &pins, "zebra").is_empty());
        assert!(search(&reg, &pins, "   ").is_empty());
    }

    #[test]
    fn search_finds_the_short_hash_and_the_pasted_label() {
        let reg = registry(&[
            (MARCEL, "Marcel Adingbatponder", 2, 100),
            (OTHER, "Solar Nord", 1, 90),
        ]);
        let pins = PinStore::new();
        for query in [
            "fa01",
            "FA01",
            "·fa01",
            "Marcel Adingbatponder ·fa01",
            "fa0123",
        ] {
            let hits = search(&reg, &pins, query);
            assert_eq!(hits.len(), 1, "{query:?}");
            assert_eq!(hits[0].dest, MARCEL, "{query:?}");
        }
        // A string past the four shown digits only matches as a hash prefix.
        assert!(search(&reg, &pins, "0123").is_empty());
    }

    #[test]
    fn a_pinned_site_not_heard_this_session_is_still_found() {
        let reg = NomadNodeRegistry::new();
        let mut pins = PinStore::new();
        pins.pin("Marcel Adingbatponder", &MARCEL, 50);
        let hits = search(&reg, &pins, "marcel");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].mark, PinMark::Pinned);
        assert_eq!(hits[0].hops, None);
        assert_eq!(hits[0].last_seen, 50);
    }

    #[test]
    fn pin_store_round_trips_through_a_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/pins.toml");
        let mut pins = PinStore::new();
        assert_eq!(
            pins.note_visit(Some("Marcel"), &MARCEL, 100),
            VisitOutcome::Pinned
        );
        assert_eq!(
            pins.note_visit(Some("Solar"), &OTHER, 110),
            VisitOutcome::Pinned
        );
        assert_eq!(
            pins.note_visit(Some("Marcel"), &MARCEL, 200),
            VisitOutcome::Touched
        );
        pins.save(&path).unwrap();

        let reloaded = PinStore::load(&path);
        assert_eq!(reloaded.list(), pins.list());
        assert_eq!(reloaded.list()[0].first_seen, 100);
        assert_eq!(reloaded.list()[0].last_seen, 200);
        assert_eq!(reloaded.dests_for("marcel"), vec![MARCEL]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[pin]]"), "{text}");
        assert!(text.contains(&format!("dest = \"{}\"", to_hex(&MARCEL))));
    }

    #[test]
    fn missing_corrupt_and_malformed_rows_load_as_no_pins() {
        let dir = tempfile::tempdir().unwrap();
        assert!(PinStore::load(&dir.path().join("absent.toml"))
            .list()
            .is_empty());
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "this is = not [valid").unwrap();
        assert!(PinStore::load(&bad).list().is_empty());
        let short = dir.path().join("short.toml");
        std::fs::write(
            &short,
            "[[pin]]\nname = \"x\"\ndest = \"fa01\"\nfirst_seen = 1\nlast_seen = 1\n",
        )
        .unwrap();
        assert!(PinStore::load(&short).list().is_empty());
    }

    #[test]
    fn a_visit_never_pins_a_second_hash_under_a_pinned_name() {
        let mut pins = PinStore::new();
        pins.note_visit(Some("Marcel"), &MARCEL, 100);
        // Someone else announcing the same name is not pinned by a visit.
        assert_eq!(
            pins.note_visit(Some("marcel"), &IMPOSTOR, 120),
            VisitOutcome::Unchanged
        );
        assert_eq!(pins.dests_for("Marcel"), vec![MARCEL]);
        // Nor is a nameless site.
        assert_eq!(pins.note_visit(None, &OTHER, 130), VisitOutcome::Unchanged);
        assert_eq!(pins.list().len(), 1);
    }

    #[test]
    fn a_duplicate_name_is_warned_sorted_below_and_pinned_only_when_confirmed() {
        let reg = registry(&[(IMPOSTOR, "Marcel", 1, 300), (MARCEL, "Marcel", 3, 200)]);
        let mut pins = PinStore::new();
        pins.note_visit(Some("Marcel"), &MARCEL, 100);

        let hits = search(&reg, &pins, "marcel");
        assert_eq!(hits.len(), 2);
        // The pinned one heads the list although the other was heard later.
        assert_eq!((hits[0].dest, &hits[0].mark), (MARCEL, &PinMark::Pinned));
        assert_eq!(
            (hits[1].dest, &hits[1].mark),
            (IMPOSTOR, &PinMark::Conflict)
        );
        // Two sites wear the name: nothing opens without the list.
        assert!(direct_hit(&hits, "Marcel").is_none());

        // Confirmed: the second hash is pinned beside the first, nothing replaced.
        assert!(pins.pin("Marcel", &IMPOSTOR, 400));
        assert_eq!(pins.dests_for("marcel"), vec![MARCEL, IMPOSTOR]);
        assert_eq!(pins.list().len(), 2);
        let hits = search(&reg, &pins, "marcel");
        assert!(hits.iter().all(|h| h.mark == PinMark::Pinned));
    }

    #[test]
    fn a_renamed_site_shows_the_name_it_was_pinned_as() {
        let reg = registry(&[(MARCEL, "Marcel II", 2, 300)]);
        let mut pins = PinStore::new();
        pins.note_visit(Some("Marcel"), &MARCEL, 100);
        let hits = search(&reg, &pins, "marcel");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].mark, PinMark::PinnedAs("Marcel".to_string()));
        assert_eq!(hits[0].name.as_deref(), Some("Marcel II"));
    }

    #[test]
    fn result_rows_carry_label_hops_age_and_mark() {
        let reg = registry(&[
            (IMPOSTOR, "Marcel", 1, 10_000 - 30),
            (MARCEL, "Marcel", 3, 10_000 - 7200),
            (OTHER, "Marcel Solar", 2, 10_000 - 600),
        ]);
        let mut pins = PinStore::new();
        pins.note_visit(Some("Marcel"), &MARCEL, 10);
        let hits = search(&reg, &pins, "marcel");
        let rows: Vec<String> = hits.iter().map(|h| result_row(h, &pins, 10_000)).collect();
        assert_eq!(
            rows,
            [
                "Marcel ·fa01  ·  3 hops  ·  2 h ago  pinned",
                "Marcel Solar ·1717  ·  2 hops  ·  10 min ago",
                "Marcel ·9c9c  ·  1 hop  ·  just now  ! name pinned to ·fa01",
            ]
        );
        assert_eq!(
            conflict_prompt(&hits[2], &pins),
            "\"Marcel\" is pinned to ·fa01; this is ·9c9c, another key. Open and pin it too? y/n"
        );
        assert_eq!(ago(200_000, 0), "2 d ago");
    }

    #[test]
    fn one_exact_unambiguous_name_opens_directly() {
        let reg = registry(&[(MARCEL, "Marcel", 2, 100), (OTHER, "Marcel Two", 1, 90)]);
        let pins = PinStore::new();
        let hits = search(&reg, &pins, "marcel");
        assert_eq!(hits.len(), 2);
        assert_eq!(direct_hit(&hits, "MARCEL").map(|h| h.dest), Some(MARCEL));
        // A substring is a list, not a direct open.
        assert!(direct_hit(&hits, "marc").is_none());

        // A site wearing a pinned name under another key is never opened
        // directly: the pinned one is listed beside it, and it asks first.
        let reg = registry(&[(IMPOSTOR, "Marcel", 1, 300)]);
        let mut pins = PinStore::new();
        pins.pin("Marcel", &MARCEL, 10);
        let hits = search(&reg, &pins, "Marcel");
        let conflict = hits.iter().find(|h| h.dest == IMPOSTOR).unwrap();
        assert_eq!(conflict.mark, PinMark::Conflict);
        assert!(direct_hit(&hits, "Marcel").is_none());
    }
}
