//! Per-collection entry counts for a [`Storage`](crate::traits::Storage) impl.
//!
//! A node could report the contents of seven tables and the size of none of
//! the rest. `MemoryStorage` holds twenty collections; `lnstatus --tables`
//! dumped the rows of seven of them and said nothing at all about the other
//! thirteen — including the packet dedup cache, which is by far the largest
//! and the only structure in the daemon that frees an entire generation in one
//! step. A resident set that steps up and falls back in 100 MB blocks cannot
//! be attributed to a structure whose size the daemon will not state.
//!
//! This module is the vocabulary for that statement: one
//! [`CollectionCount`] per collection, carrying the live entry count and, when
//! the collection is held to a configured ceiling, that ceiling beside it. A
//! count without its ceiling does not answer the question an operator has,
//! which is not "how many" but "how close to full".
//!
//! # Cost
//!
//! Every count here is `len()` on a `BTreeMap`, `BTreeSet`, `VecDeque`,
//! `HashSet` or a heapless map, all O(1) — a census is a handful of loads, so
//! it is free to take on every status call. The one exception is
//! `MemoryStorage::local_client_dest_map`, a map of sets whose entry count is
//! the sum over the inner sets: O(number of local interfaces), which is a
//! single-digit number on every node we run. Nothing here walks entries.
//!
//! # Capacities are reported, never enforced here
//!
//! `capacity` is what the owning storage is already enforcing elsewhere. This
//! module introduces no ceiling and changes none; a `None` means the
//! collection genuinely has no configured bound, which is a fact worth seeing
//! rather than a gap to paper over (Codeberg #421 decides which of those
//! deserve one, and needs these counts first).

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// One collection's live size, named as the field that holds it.
///
/// `name` is the struct field name on purpose: it is the one identifier that
/// a reader of the daemon's output and a reader of the source can both use,
/// and it is what the binding test in each storage module compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionCount {
    /// Field name of the collection in its storage struct.
    pub name: &'static str,
    /// Live entry count.
    pub entries: usize,
    /// Configured ceiling the owning storage holds this collection to, or
    /// `None` when it has none. For a rotating cache this is the threshold
    /// that triggers the rotation, i.e. the ceiling of one generation.
    pub capacity: Option<usize>,
}

impl CollectionCount {
    /// A collection with no configured ceiling.
    pub const fn unbounded(name: &'static str, entries: usize) -> Self {
        Self {
            name,
            entries,
            capacity: None,
        }
    }

    /// A collection held to `capacity` entries.
    pub const fn bounded(name: &'static str, entries: usize, capacity: usize) -> Self {
        Self {
            name,
            entries,
            capacity: Some(capacity),
        }
    }
}

/// Field names of every collection-typed field of `struct_name` in `source`.
///
/// This is how a census is bound to the struct it claims to describe. Rust has
/// no reflection, so the alternative is a hand-kept list with nothing checking
/// it — which is exactly how thirteen collections came to be invisible. A test
/// feeds this the `include_str!` of the module that defines the struct and
/// asserts the two lists agree, so a field added without a counter fails a
/// test instead of quietly going unreported.
///
/// Returns `None` when `struct_name` is not found in `source`, so a test
/// cannot pass vacuously against a renamed or moved struct.
///
/// The parse is deliberately dumb: strip attributes and comments, split the
/// body on commas that are not inside `<>`, `[]` or `()`, and keep the fields
/// whose type mentions a collection type. It understands the declaration
/// styles this repository actually uses, and a type it cannot classify shows
/// up as a missing name, never as a silent pass.
pub fn collection_fields(source: &str, struct_name: &str) -> Option<Vec<String>> {
    let body = struct_body(source, struct_name)?;
    let mut fields = Vec::new();
    for field in split_top_level(&body) {
        let Some((name, ty)) = field.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !is_ident(name) {
            continue;
        }
        if COLLECTION_TYPES.iter().any(|t| ty.contains(t)) {
            fields.push(name.to_string());
        }
    }
    Some(fields)
}

/// Type names that make a field a collection for census purposes. A field
/// whose type mentions one of these holds an unbounded-in-principle number of
/// entries and therefore owes the report a count.
const COLLECTION_TYPES: [&str; 11] = [
    "BoundedMap",
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "HashMap",
    "HashSet",
    "IndexMap",
    "IndexSet",
    "OrderedMap",
    "OrderedSet",
    "Vec<",
];

/// The `{ ... }` body of `struct <struct_name>`, with comments and attributes
/// removed and newlines flattened to spaces.
fn struct_body(source: &str, struct_name: &str) -> Option<String> {
    let mut cleaned = String::new();
    let mut in_struct = false;
    let mut depth = 0usize;
    for raw in source.lines() {
        let line = strip_comment(raw).trim();
        if !in_struct {
            // `pub struct Name {`, `struct Name<K, V, const N: usize>` ...
            let Some((_, after)) = line.split_once("struct ") else {
                continue;
            };
            let head = after.trim_start();
            if !head.starts_with(struct_name) {
                continue;
            }
            let rest = &head[struct_name.len()..];
            if !rest.starts_with(|c: char| c.is_whitespace() || c == '{' || c == '<') {
                continue;
            }
            in_struct = true;
            if let Some((_, tail)) = line.split_once('{') {
                depth = 1;
                push_line(&mut cleaned, tail);
            }
            continue;
        }
        if depth == 0 {
            if let Some((_, tail)) = line.split_once('{') {
                depth = 1;
                push_line(&mut cleaned, tail);
            }
            continue;
        }
        if line.starts_with("#[") {
            continue;
        }
        if line == "}" {
            return Some(cleaned);
        }
        push_line(&mut cleaned, line);
    }
    None
}

fn push_line(out: &mut String, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push(' ');
    }
    out.push_str(line);
}

/// Everything before a `//` that is not inside a string literal. No field
/// declaration in this repository carries a `//` inside a literal, and a
/// comment containing a collection type name is otherwise indistinguishable
/// from a declaration.
fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Split a struct body on commas at bracket depth zero, so a comma inside
/// `BTreeMap<K, V>` or `[u8; N]` does not cut a field in half.
fn split_top_level(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in body.chars() {
        match c {
            '<' | '[' | '(' => {
                depth += 1;
                current.push(c);
            }
            '>' | ']' | ')' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => {
                out.push(core::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

fn is_ident(s: &str) -> bool {
    let s = s.strip_prefix("pub ").unwrap_or(s).trim();
    let s = match s.find(')') {
        // `pub(crate) name`
        Some(idx) => s[idx + 1..].trim(),
        None => s,
    };
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
/// A doc comment mentioning BTreeMap should not become a field.
pub struct Sample {
    // A plain comment, also mentioning VecDeque.
    scalar: usize,
    /// Doc.
    map: BTreeMap<[u8; 16], (usize, u64)>,
    #[cfg(test)]
    hook: Option<Hook>,
    ring: VecDeque<[u8; 32]>,
    nested: BTreeMap<usize, BTreeSet<[u8; 16]>>,
    name: String,
}

pub struct Other {
    other_map: BTreeMap<u8, u8>,
}
"#;

    #[test]
    fn collection_fields_finds_the_collections_and_only_those() {
        let fields = collection_fields(SAMPLE, "Sample").expect("Sample is in the source");
        assert_eq!(fields, ["map", "ring", "nested"]);
    }

    #[test]
    fn collection_fields_stops_at_the_named_struct() {
        let fields = collection_fields(SAMPLE, "Other").expect("Other is in the source");
        assert_eq!(fields, ["other_map"]);
    }

    /// A struct that is not there is not an empty struct: a test that binds a
    /// census to a renamed struct must fail, not pass with nothing to compare.
    #[test]
    fn collection_fields_reports_a_missing_struct() {
        assert!(collection_fields(SAMPLE, "Absent").is_none());
    }

    #[test]
    fn capacity_is_present_only_when_configured() {
        assert_eq!(CollectionCount::unbounded("a", 3).capacity, None);
        assert_eq!(CollectionCount::bounded("b", 3, 9).capacity, Some(9));
    }
}
