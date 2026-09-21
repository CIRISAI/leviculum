//! `EVENT_CATALOG` completeness, checked against the tree rather than
//! against a habit.
//!
//! # Why a source scan and not a runtime check
//!
//! The event-log layer validates an event's shape only for a name it
//! finds in [`EVENT_CATALOG`] (`event_log.rs`, `on_event`). An
//! undeclared name is therefore not a cosmetic gap: it is an event
//! whose schema check can never run, so it can lose a required field
//! forever and nothing says a word.
//!
//! That is not a hypothetical. The miauhaus soak of 2026-09-18
//! (397 023 881 events, two months of public mesh) found eight emitted
//! names missing from the catalogue, `LINK_ENTRY_SET` among them at
//! 612 639 emissions — and what caught `LINK_ENTRY_SET`'s broken
//! `next_hop`, 252 669 times, was the field-VALUE check, which runs for
//! every event regardless of the catalogue. The schema check, the one
//! that would have named the field, could not run at all.
//!
//! A runtime check can only see what the running deployment happens to
//! emit; the soak found eight because that node's traffic exercised
//! eight. This test reads the tree, so a name is caught the day its
//! call site is written, whether or not anything has emitted it yet.
//!
//! # What is scanned
//!
//! Every `.rs` file under `src/` of every member listed in the
//! workspace `Cargo.toml`, with two exclusions and no allowlist:
//!
//! - `#[cfg(test)]` modules, inline or in their own file. A test's
//!   fixture event has no production contract to keep.
//! - Comments, stripped before scanning, so the `event = "FOO"` in
//!   this crate's own doc comments is not mistaken for a call site.
//!
//! The firmware crates (`leviculum-nrf`, `leviculum-esp`) are not
//! workspace members and so are not scanned, which is the right answer
//! for a second reason: they are `no_std`, emit their line grammar
//! without a `tracing` subscriber, and `docs/src/structured-event-
//! logs.md` states that firmware events deliberately do NOT appear in
//! `EVENT_CATALOG` — a catalogue entry the subscriber can never see
//! emitted is the stale-catalogue rot the same document forbids.
//!
//! # The second rule: no free-text message on an event site
//!
//! `tracing::debug!(event = "FOO", a = 1, "some prose")` renders the
//! prose under a `message` field, and its spaces split the line for
//! every token-based parser (`jl`, `jldiff`, the periculum cells' awk).
//! A catalogued event with a `message` field is a broken line by
//! construction. The same scan catches it, because the same walk that
//! finds the event name sees the trailing literal.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::event_log::EVENT_CATALOG;

/// The `tracing` macros an event site can be raised through.
const LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

/// One `tracing::*!(event = "NAME", ...)` call site.
#[derive(Debug)]
struct Site {
    event: String,
    file: String,
    line: usize,
    /// A string literal at the macro's top level that is not a field
    /// value — i.e. the format/message argument.
    message: bool,
}

// ---------------------------------------------------------------------
// Workspace discovery
// ---------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("leviculum-std sits one level below the workspace root")
        .to_path_buf()
}

/// Members of the `[workspace]` table, read from the root manifest so a
/// new crate is scanned the day it is added.
fn workspace_members(root: &Path) -> Vec<String> {
    let manifest =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace Cargo.toml is readable");
    let after = manifest
        .split_once("members = [")
        .expect("workspace Cargo.toml declares members")
        .1;
    let list = after.split_once(']').expect("the members list is closed").0;
    let members: Vec<String> = list
        .lines()
        .filter_map(|l| {
            let l = l.trim().trim_end_matches(',').trim();
            let l = l.strip_prefix('"')?.strip_suffix('"')?;
            Some(l.to_string())
        })
        .collect();
    assert!(
        members.len() > 5,
        "parsed {} workspace members, which cannot be right: {members:?}",
        members.len()
    );
    members
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------
// Lexing: comments out, literals intact
// ---------------------------------------------------------------------

/// Replace every comment with spaces, preserving byte offsets and line
/// breaks so reported line numbers stay true.
///
/// String, raw-string and char literals are walked through rather than
/// over: a `//` inside a literal is not a comment, and a `"` inside a
/// char literal does not open one.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                // Rust block comments nest.
                let mut depth = 0usize;
                while i < b.len() {
                    if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                        depth += 1;
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                    } else if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                        depth -= 1;
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        if b[i] != b'\n' {
                            out[i] = b' ';
                        }
                        i += 1;
                    }
                }
            }
            _ => i = skip_literal(b, i).unwrap_or(i + 1),
        }
    }
    String::from_utf8(out).expect("replacing comment bytes with spaces keeps UTF-8 valid")
}

/// If a literal starts at `i`, return the index just past it.
///
/// Handles `"..."`, `r"..."` / `r#"..."#` (and their `b` forms) and
/// `'c'`. A `'` that opens a lifetime is not a literal and returns
/// `None`, so the caller advances by one byte as usual.
fn skip_literal(b: &[u8], i: usize) -> Option<usize> {
    let prev_is_ident = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');

    // Raw string: r"..." / r#"..."# / br#"..."#
    let raw_start = match b[i] {
        b'r' if !prev_is_ident => Some(i + 1),
        b'b' if !prev_is_ident && b.get(i + 1) == Some(&b'r') => Some(i + 2),
        _ => None,
    };
    if let Some(mut j) = raw_start {
        let hash_start = j;
        while b.get(j) == Some(&b'#') {
            j += 1;
        }
        let hashes = j - hash_start;
        if b.get(j) == Some(&b'"') {
            j += 1;
            loop {
                match b.get(j) {
                    None => return Some(b.len()),
                    Some(b'"') => {
                        let closed = (1..=hashes).all(|k| b.get(j + k) == Some(&b'#'));
                        if closed {
                            return Some(j + 1 + hashes);
                        }
                        j += 1;
                    }
                    Some(_) => j += 1,
                }
            }
        }
        return None;
    }

    if b[i] == b'"' {
        let mut j = i + 1;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'"' => return Some(j + 1),
                _ => j += 1,
            }
        }
        return Some(b.len());
    }

    if b[i] == b'\'' {
        // `'a'` or `'\n'`; anything else is a lifetime.
        if b.get(i + 1) == Some(&b'\\') {
            let mut j = i + 2;
            while j < b.len() && b[j] != b'\'' {
                j += 1;
            }
            return Some((j + 1).min(b.len()));
        }
        if b.get(i + 2) == Some(&b'\'') {
            return Some(i + 3);
        }
        return None;
    }

    None
}

// ---------------------------------------------------------------------
// Skipping `#[cfg(test)]`
// ---------------------------------------------------------------------

/// Module names declared as `#[cfg(test)] mod <name>;` (or
/// `#[cfg(all(test, ...))]`) anywhere in the scanned sources. Their
/// files are test-only and are not scanned.
fn cfg_test_module_names(texts: &BTreeMap<PathBuf, String>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for text in texts.values() {
        for (attr_end, _) in cfg_test_attrs(text) {
            let rest = text[attr_end..].trim_start();
            let Some(rest) = rest.strip_prefix("mod ") else {
                continue;
            };
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let tail = rest.trim_start()[name.len()..].trim_start();
            if !name.is_empty() && tail.starts_with(';') {
                names.insert(name);
            }
        }
    }
    names
}

/// Byte ranges of every inline `#[cfg(test)] mod ... { ... }` in `text`.
///
/// Only `mod` is skipped, deliberately. A `#[cfg(test)]` also sits on
/// struct fields, `impl` blocks and free functions, and a rule that
/// hunted for "the next brace" after any of them would swallow the item
/// that follows — which is how a first draft of this test skipped all of
/// `transport.rs` and reported a clean catalogue. Skipping too little
/// costs at worst a declaration for a test-only event; skipping too much
/// costs the whole point of the check.
fn cfg_test_block_ranges(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut ranges = Vec::new();
    for (attr_end, attr_start) in cfg_test_attrs(text) {
        let mut i = attr_end;
        // Further attributes may sit between the cfg and the item.
        loop {
            while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                i += 1;
            }
            if b.get(i) == Some(&b'#') && b.get(i + 1) == Some(&b'[') {
                let mut depth = 0usize;
                while i < b.len() {
                    match b[i] {
                        b'[' => {
                            depth += 1;
                            i += 1;
                        }
                        b']' => {
                            depth -= 1;
                            i += 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => i = skip_literal(b, i).unwrap_or(i + 1),
                    }
                }
                continue;
            }
            break;
        }
        if !text[i..].starts_with("mod ") {
            continue;
        }
        // Walk to the module's opening brace; a `;` first is a file
        // module, handled by `cfg_test_module_names`.
        while i < b.len() && b[i] != b'{' && b[i] != b';' {
            i = skip_literal(b, i).unwrap_or(i + 1);
        }
        if i >= b.len() || b[i] == b';' {
            continue;
        }
        let mut depth = 0usize;
        while i < b.len() {
            match b[i] {
                b'{' => {
                    depth += 1;
                    i += 1;
                }
                b'}' => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => i = skip_literal(b, i).unwrap_or(i + 1),
            }
        }
        ranges.push((attr_start, i));
    }
    ranges
}

/// `(index just past the attribute, index of its `#`)` for every
/// `#[cfg(...)]` whose predicate mentions `test`.
fn cfg_test_attrs(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut search = 0usize;
    while let Some(rel) = text[search..].find("#[cfg(") {
        let start = search + rel;
        let open = start + "#[cfg".len();
        let mut i = open;
        let mut depth = 0usize;
        while i < b.len() {
            match b[i] {
                b'(' => {
                    depth += 1;
                    i += 1;
                }
                b')' => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => i += 1,
            }
        }
        // `test` as a whole word, outside any string: `cfg(test)` and
        // `cfg(all(test, feature = "tracing"))` count, the feature name in
        // `cfg(feature = "test-util")` does not.
        let predicate: String = {
            let raw = &text[open..i];
            let mut kept = String::new();
            let mut in_str = false;
            let mut escaped = false;
            for c in raw.chars() {
                match c {
                    _ if escaped => escaped = false,
                    '\\' if in_str => escaped = true,
                    '"' => in_str = !in_str,
                    _ if !in_str => kept.push(c),
                    _ => {}
                }
            }
            kept
        };
        let mentions_test = predicate
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|w| w == "test");
        if mentions_test && b.get(i) == Some(&b']') {
            out.push((i + 1, start));
        }
        search = start + 1;
    }
    out
}

// ---------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------

fn scan(file: &str, text: &str) -> Vec<Site> {
    let skip = cfg_test_block_ranges(text);
    let b = text.as_bytes();
    let mut sites = Vec::new();
    let mut search = 0usize;

    while let Some(rel) = text[search..].find("tracing::") {
        let at = search + rel;
        search = at + 1;
        if skip.iter().any(|(s, e)| at >= *s && at < *e) {
            continue;
        }
        let after = at + "tracing::".len();
        let Some(level) = LEVELS.iter().find(|l| text[after..].starts_with(**l)) else {
            continue;
        };
        let mut i = after + level.len();
        if b.get(i) != Some(&b'!') {
            continue;
        }
        i += 1;
        while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
            i += 1;
        }
        if b.get(i) != Some(&b'(') {
            continue;
        }

        // Walk the macro body, tracking nesting across all three
        // bracket kinds: a field value may be a `match { ... }` arm or
        // an `if ... { "ok" } else { "bad" }`, and a literal in there
        // is a value, not the macro's message argument.
        let body_open = i;
        let mut depth = 0usize;
        let mut event: Option<String> = None;
        let mut message = false;
        while i < b.len() {
            match b[i] {
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    i += 1;
                }
                b')' | b']' | b'}' => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {
                    if depth == 1 && b[i] == b'"' {
                        let end = skip_literal(b, i).unwrap_or(i + 1);
                        // A field value is preceded by `=`; anything
                        // else at the macro's top level is the
                        // message/format argument.
                        let mut k = i;
                        while k > body_open && b[k - 1].is_ascii_whitespace() {
                            k -= 1;
                        }
                        if k == body_open || b[k - 1] != b'=' {
                            message = true;
                        }
                        i = end;
                        continue;
                    }
                    if depth == 1 && text[i..].starts_with("event") {
                        let before_ok = !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
                        let mut k = i + "event".len();
                        while b.get(k).is_some_and(|c| c.is_ascii_whitespace()) {
                            k += 1;
                        }
                        if before_ok && b.get(k) == Some(&b'=') {
                            k += 1;
                            while b.get(k).is_some_and(|c| c.is_ascii_whitespace()) {
                                k += 1;
                            }
                            if b.get(k) == Some(&b'"') {
                                let end = skip_literal(b, k).unwrap_or(k + 1);
                                event = Some(text[k + 1..end - 1].to_string());
                                i = end;
                                continue;
                            }
                        }
                    }
                    i = skip_literal(b, i).unwrap_or(i + 1);
                }
            }
        }

        if let Some(event) = event {
            sites.push(Site {
                event,
                file: file.to_string(),
                line: text[..at].matches('\n').count() + 1,
                message,
            });
        }
    }
    sites
}

fn all_sites() -> Vec<Site> {
    let root = workspace_root();
    let mut files = Vec::new();
    for member in workspace_members(&root) {
        rust_files(&root.join(&member).join("src"), &mut files);
    }
    assert!(
        files.len() > 50,
        "found only {} source files under the workspace members",
        files.len()
    );

    let texts: BTreeMap<PathBuf, String> = files
        .into_iter()
        .map(|p| {
            let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"));
            (p, strip_comments(&raw))
        })
        .collect();

    let test_only = cfg_test_module_names(&texts);
    let mut sites = Vec::new();
    for (path, text) in &texts {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let module = if stem == "mod" {
            path.parent()
                .and_then(|d| d.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        } else {
            stem
        };
        if test_only.contains(&module) {
            continue;
        }
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        sites.extend(scan(&rel, text));
    }
    sites
}

// ---------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------

/// The scanner has to be shown working before its silence means
/// anything: a check that finds nothing because it parses nothing
/// passes forever.
#[test]
fn scanner_finds_the_sites_it_is_meant_to_find() {
    let sites = all_sites();
    // A floor, not a fingerprint: the tree carried about 130 event sites
    // when this was written, and the named checks below are what actually
    // prove the walk reaches every crate.
    assert!(
        sites.len() > 80,
        "scanned the workspace and found only {} event sites",
        sites.len()
    );

    let names: BTreeSet<&str> = sites.iter().map(|s| s.event.as_str()).collect();
    // A handful of names from four different crates, so a scanner that
    // silently stops after one member is caught here.
    for expected in [
        "PKT_RX",            // leviculum-core/src/transport.rs
        "LINK_ENTRY_SET",    // leviculum-core/src/transport.rs
        "RESOURCE_TX_STATE", // leviculum-core/src/resource/outgoing.rs
        "BLE_LINK_UP",       // leviculum-std/src/interfaces/ble/mod.rs
        "REENTRANT_LOCK",    // leviculum-std/src/sync_ext.rs
        "HELPER_TICK",       // leviculum-std/src/bin/event-log-helper.rs
        "LNMSG_DONE",        // lnmsg/src/events.rs
        "PN_SYNC",           // lnpnd/src/peering.rs
    ] {
        assert!(
            names.contains(expected),
            "scanner missed {expected}; it found {} names: {names:?}",
            names.len()
        );
    }

    // And the exclusions hold: `FOO` lives only in this crate's doc
    // comment for "how to add an event", `NAME` only in a `#[cfg(test)]`
    // module's doc comment.
    for absent in ["FOO", "NAME"] {
        assert!(
            !names.contains(absent),
            "{absent} is documentation, not a call site; comment stripping failed"
        );
    }
}

/// Every event name the workspace emits is declared in `EVENT_CATALOG`.
#[test]
fn every_emitted_event_is_catalogued() {
    let declared: BTreeSet<&str> = EVENT_CATALOG.iter().map(|s| s.name).collect();
    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for site in all_sites() {
        if !declared.contains(site.event.as_str()) {
            missing
                .entry(site.event.clone())
                .or_default()
                .push(format!("{}:{}", site.file, site.line));
        }
    }
    assert!(
        missing.is_empty(),
        "these event names are emitted but not declared in EVENT_CATALOG \
         (leviculum-std/src/event_log.rs). An undeclared event is never \
         schema-checked, so it can lose a required field silently -- declare \
         it with the shape its emitters actually produce:\n{}",
        missing
            .iter()
            .map(|(name, at)| format!("  {name}  <- {}", at.join(", ")))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// No event site carries a free-text message.
///
/// `tracing::debug!(event = "FOO", a = 1, "some prose")` renders the
/// prose under a `message` field whose spaces split the line for every
/// token-based parser. The soak of 2026-09-20 caught exactly this on
/// `TUNNEL_PATH_ASSOCIATED`, as `EVENT_FIELD_VIOLATION field=message
/// value_problem=whitespace`.
#[test]
fn no_event_site_carries_a_free_text_message() {
    let offenders: Vec<String> = all_sites()
        .into_iter()
        .filter(|s| s.message)
        .map(|s| format!("  {}  at {}:{}", s.event, s.file, s.line))
        .collect();
    assert!(
        offenders.is_empty(),
        "these event sites pass a message/format argument to the tracing \
         macro. It renders as a `message` field whose whitespace splits the \
         line; move what it said into a structured field, or into a comment \
         at the call site:\n{}",
        offenders.join("\n")
    );
}

/// The same rule stated on the catalogue side: a declared event that
/// requires a `message` key is a broken line by construction.
#[test]
fn no_catalogue_entry_requires_a_message_key() {
    let offenders: Vec<&str> = EVENT_CATALOG
        .iter()
        .filter(|s| s.required_keys.contains(&"message"))
        .map(|s| s.name)
        .collect();
    assert!(
        offenders.is_empty(),
        "EVENT_CATALOG entries requiring a `message` key: {offenders:?}"
    );
}
