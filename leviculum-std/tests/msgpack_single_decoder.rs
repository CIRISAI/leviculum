//! Single-decoder guard: `leviculum-core` must carry exactly one hand-rolled
//! msgpack reader, and it must not spell a bounds check by hand.
//!
//! Codeberg #302. Until this guard existed there were two: one under
//! `resource/`, taking bytes off the wire, and a second copied into
//! `destination.rs` whose header comment recorded that it followed the
//! first's pattern. #267 was a wrapping bounds guard — `*pos + len >
//! data.len()` with `len` a full `u32` off the wire, which wraps below
//! `data.len()` on the 32-bit firmware target and lets a panicking slice
//! through. Fixing it meant fixing it twice, in two files, and the second
//! copy was found only because that batch happened to include an explicit
//! audit for the shape. Nothing would have found it otherwise: the issue
//! named one file and the reported panic was in one file.
//!
//! The two copies were also not reachable the same way, which makes "they
//! are the same, so one fix covers both" wrong in a way that is easy to
//! assume — the resource decoder reads the wire, the destination one parses
//! the ratchet store's outer map *before* the Ed25519 signature over it is
//! verified.
//!
//! Two checks, matching the two ways the defect showed up:
//!
//! 1. **One home for the primitive readers.** The functions that consume
//!    msgpack tag and length bytes are named in [`PRIMITIVE_READERS`] and may
//!    be defined only in [`DECODER`]. Wrappers elsewhere that delegate to
//!    them (`read_str_or_nil`, `read_bin_array`, …) are fine and are not
//!    matched: they carry no tag table, so a tag-table defect cannot hide in
//!    one.
//! 2. **No hand-spelled cursor arithmetic.** `*pos + n` in a bounds check or
//!    a slice index is precisely the #267 shape. Every reader advances
//!    through `take`, which uses `checked_add`, so the sum cannot wrap
//!    whatever width `usize` is on the target.

use std::fs;
use std::path::{Path, PathBuf};

/// The one module allowed to decode msgpack tag bytes, repo-relative.
const DECODER: &str = "leviculum-core/src/msgpack.rs";

/// The crate whose sources this guard walks. `leviculum-lxmf` has its own
/// msgpack module for the LXMF field format and is deliberately out of
/// scope: it is a different crate with a different payload grammar, and
/// folding it in here would be a wire-format decision, not a cleanup.
const GUARDED_SRC: &str = "leviculum-core/src";

/// The parameter that makes a function a msgpack cursor reader rather than
/// something that merely shares a name with one. Required alongside the name,
/// because the crate legitimately has a `Clock::advance(ms)`, a segment
/// plan's `advance()`, and an RNode framing `read_be_u16(data, offset)` —
/// none of which touch a msgpack tag table.
const CURSOR_PARAM: &str = "pos: &mut usize";

/// Functions that read a msgpack tag or a length off the buffer. A second
/// definition of any of these is a second decoder, by definition: each one
/// owns part of the tag table.
const PRIMITIVE_READERS: &[&str] = &[
    "take",
    "advance",
    "read_byte",
    "read_be_u16",
    "read_be_u32",
    "read_be_u64",
    "read_bool",
    "read_float64",
    "read_map_len",
    "read_array_len",
    "read_fixmap_len",
    "read_fixarray_len",
    "read_msgpack_str",
    "read_msgpack_bin",
    "read_msgpack_uint",
    "read_msgpack_bin_or_nil",
    "read_msgpack_raw_value",
    "read_msgpack_array_len",
    "skip_msgpack_value",
    "skip_msgpack_value_depth",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

/// Every `.rs` file under `dir`, recursively, repo-relative.
fn rust_sources(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(
                path.strip_prefix(root)
                    .expect("source is under the repo root")
                    .to_path_buf(),
            );
        }
    }
}

/// `fn <name>(` or `fn <name><'a>(` at a definition site, for one of the
/// primitive reader names, taking the msgpack cursor. Matched on the name
/// boundary so `read_str` does not match `read_str_or_nil`.
fn defines_primitive_reader(line: &str) -> Option<&'static str> {
    if !line.contains(CURSOR_PARAM) {
        return None;
    }
    let rest = line.split_once("fn ")?.1;
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let (name, after) = rest.split_at(name_end);
    // A definition's name is followed by its parameter list or its
    // lifetime/generic list; anything else is a call or a doc reference.
    if !after.starts_with('(') && !after.starts_with('<') {
        return None;
    }
    PRIMITIVE_READERS.iter().copied().find(|&r| r == name)
}

#[test]
fn primitive_msgpack_readers_are_defined_once() {
    let root = repo_root();
    let mut sources = Vec::new();
    rust_sources(&root, &root.join(GUARDED_SRC), &mut sources);
    assert!(!sources.is_empty(), "{GUARDED_SRC} has no .rs files");

    let decoder = Path::new(DECODER);
    assert!(
        sources.iter().any(|p| p == decoder),
        "{DECODER} does not exist — the shared decoder moved, so this guard \
         is pointing at nothing and would pass vacuously"
    );

    let mut findings = Vec::new();
    for path in &sources {
        if path == decoder {
            continue;
        }
        let text = fs::read_to_string(root.join(path))
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (idx, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(name) = defines_primitive_reader(line) {
                findings.push(format!(
                    "{}:{}: defines `{name}`, a msgpack primitive reader that \
                     belongs in {DECODER}",
                    path.display(),
                    idx + 1,
                ));
            }
        }
    }

    assert!(
        findings.is_empty(),
        "a second msgpack decoder has appeared; a defect fixed in {DECODER} \
         would not be fixed here (Codeberg #302, #267):\n  {}",
        findings.join("\n  "),
    );
}

#[test]
fn no_hand_spelled_cursor_arithmetic() {
    let root = repo_root();
    let mut sources = Vec::new();
    rust_sources(&root, &root.join(GUARDED_SRC), &mut sources);

    let mut findings = Vec::new();
    for path in &sources {
        let text = fs::read_to_string(root.join(path))
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (idx, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            // `*pos + ` (plus, then space) is the arithmetic; `*pos += 1` is
            // an advance and has no space between `+` and `=`.
            if line.contains("*pos + ") {
                findings.push(format!("{}:{}: {}", path.display(), idx + 1, line.trim(),));
            }
        }
    }

    assert!(
        findings.is_empty(),
        "`*pos + n` computed by hand: on a 32-bit `usize` a wire-supplied \
         length wraps the sum below `data.len()`, so the bounds check passes \
         and the slice behind it panics (Codeberg #267). Advance through \
         `take`, which uses `checked_add`:\n  {}",
        findings.join("\n  "),
    );
}
