//! Citation guard (Guarantee C): a reference to something outside the text
//! must still point at what it claims. Covers `docs/src/**/*.md`, the
//! Rust sources and tests of `leviculum-core`, `leviculum-lxmf`,
//! `leviculum-lxmf-node` and `leviculum-std`, and the gate scripts under
//! `scripts/` together with the `Justfile`; and every concept document
//! must be reachable from `docs/src/SUMMARY.md`.
//!
//! The concept documents are binding policy, and a wrong citation gets
//! believed. A 2026-07 audit found six drifted citations across five
//! documents after roughly one month; nothing else catches this, because
//! a drifted citation looks exactly like a fresh one. Source citations are
//! the load-bearing half: a pinned deviation means nothing without the
//! reference line it deviates from.
//!
//! # Three kinds of sentence, and which of them is checked
//!
//! A reader of a doc comment cannot tell by looking which of these they are
//! in front of, so this is the map.
//!
//! **1. A `path:line` citation — resolved.** Per citation:
//! - the line spec reads forwards: a range whose end precedes its start is
//!   refused before anything is resolved, because no state of the tree can
//!   make it right and [`repaired`] will not rewrite one;
//! - the cited file exists in the repo and has at least the cited number
//!   of lines (catches deletions and renames);
//! - where the citation *names what it points at* — a backticked
//!   identifier attached to it, see below — that identifier occurs within
//!   `WINDOW` lines of the cited span (catches drift). In a `Justfile` the
//!   name has to be *defined* at the cited line, not merely occur there:
//!   see [`defines_recipe`].
//!
//! A bare citation that names nothing only gets the existence check. Both
//! kinds are counted and printed so the coverage is visible: run with
//! `--nocapture` to see the counts.
//!
//! ## What "names what it points at" means
//!
//! A line number in a moving tree is not a durable anchor: every batch that
//! inserts a line above the cited one silently repoints the citation at
//! whatever now sits there. Existence-checking a line number cannot see
//! that, because the line still exists — it is simply about something else.
//! The name is what survives the move, so a citation that carries one can
//! be checked and a citation that carries none cannot.
//!
//! Three spellings count as attaching a name, and all three are what the
//! corpus already writes:
//!
//! ```text
//! `resolve_lt_alock` (`leviculum-std/src/driver/mod.rs:513`)   -- paren
//! (`resolve_lt_alock`, `leviculum-std/src/driver/mod.rs:513`)  -- comma
//! | `fn resolve_lt_alock(&self) -> bool` — `driver/mod.rs:513` -- table
//! ```
//!
//! In the paren and comma spellings nothing but whitespace may sit between
//! the name and the citation, so the pairing is unambiguous: an identifier
//! mentioned earlier in the sentence is not read as the citation's
//! subject. A token that is itself a citation (`Destination.py:322`) or
//! that carries no letter in its last segment (a `1209:0001` USB VID:PID)
//! is not an identifier and does not attach — those sit next to citations
//! in tables and would otherwise be read as the subject of the citation
//! beside them.
//!
//! The table spelling is the exception that distance rule has to make. A
//! reference table writes the whole signature and then the citation, so
//! the name is never adjacent; requiring adjacency read none of those
//! rows, and they are the densest citations in the book. Inside a table
//! row a `fn NAME(` in a backticked span therefore names the next citation
//! on that row — and only the next one, because a citation between the two
//! takes the signature for itself and leaves this one bare. That was
//! Codeberg #307: a 17-row `ReticulumNode` method table whose citations had
//! aged past 1000 lines while the guard called the file fine, because every
//! row of it was bare.
//!
//! The comma spelling was admitted in 2026-08 after `lora.rs:1061` drifted
//! onto radio-init code inside a *regulatory* claim and this guard passed
//! it. Adding it converted 75 book and 55 source citations from
//! existence-checked to drift-checked without editing one of them, and
//! immediately reported 30 that had drifted. Which is the argument for
//! spelling the shape the tree already uses rather than inventing a new
//! one: a scheme that needs every citation rewritten by hand is a
//! migration, and gets done never.
//!
//! A citation into a `reference/` submodule that is not checked out is a
//! *different* failure from a drifted one, and says so: nothing is wrong
//! with the citation, the reference is simply absent. Whether a checked-out
//! reference is at the commit this tree pins is not checkable from here —
//! that is `scripts/check-submodule-pins.sh`, in a gate rather than in a
//! test. See `docs/src/concepts/checks-and-citations.md`.
//!
//! **2. A prose attribution to a document — checked for figures only.** A
//! Rust doc-comment paragraph that names a document under `docs/` and
//! quotes a decimal figure must have that figure occur in that document
//! (Codeberg #200). It carries no line and no identifier, so nothing above
//! reaches it: `PROCESSOR_TICK_BUDGET` was justified with "the number comes
//! off `docs/…/core-lock-budget.md`" and then named 126.6 ms, a figure that
//! existed nowhere in the tree but in that comment. The measurement was
//! real; the attribution was not.
//!
//! Only decimal figures, and only within one paragraph. See
//! `figure_attributions` for exactly what that excludes and why the trigger
//! is drawn where it is.
//!
//! **3. Everything else in a doc comment — unchecked prose.** Which is most
//! of it. A sentence can name a mechanism that no longer exists, describe a
//! guarantee the code does not make, or attribute an integer to a page that
//! never carried it, and nothing here will notice. Guarantee C is about
//! references, not about truth.

use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Identifier search window in lines around the cited span.
///
/// Measured on the corpus at introduction time (2026-08): every correct
/// citation had its identifier within 5 lines of the cited line (doc
/// comments, attributes and derives legitimately sit between a cited
/// first line and the item name), while every genuinely drifted citation
/// was off by 11 lines or more. 8 splits the measured gap with margin on
/// both sides.
const WINDOW: usize = 8;

/// Path prefixes that refer to sibling repositories. These citations are
/// counted and reported but cannot be existence-checked from this
/// workspace.
///
/// `columba/` is the phone app whose BLE behaviour we interoperate with,
/// checked out read-only beside this one (`schneckenschreck:/home/lew/
/// coding/columba`, branch `main`). It joined the list when the duplicate
/// rule started porting Columba functions line by line: a citation that
/// names the Kotlin file and line is what lets the next reader check the
/// port against the real thing instead of against a transcription.
const EXTERNAL_PREFIXES: &[&str] = &[
    "periculum/",
    "ble-reticulum/",
    "columba/",
    // A crates.io dependency, cited by the nRF gate scripts. Its source is
    // under `~/.cargo`, never in this tree, so it can only be counted.
    "nrf-softdevice/",
];

/// The vendored references. A citation into one of these that is not
/// checked out fails differently from a citation that has drifted.
const SUBMODULES: &[&str] = &["Reticulum", "LXMF", "LXST", "RNode_Firmware"];

/// Crate roots whose Rust sources carry citations. Whole crate directories,
/// not `src/` alone: `tests/`, `examples/` and `benches/` cite the reference
/// as much as `src/` does, and narrowing the glob would only make the number
/// smaller, not the tree more correct.
const SOURCE_CRATES: &[&str] = &[
    "leviculum-core",
    "leviculum-lxmf",
    "leviculum-lxmf-node",
    "leviculum-std",
];

/// Extensions that make a `name.ext:number` look like a citation while being
/// a `host:port`. Only reachable in `Corpus::Source`, where citations are not
/// backticked and a config example like `peer.example.com:5000` sits in an
/// ordinary string literal. A blocklist rather than an extension allowlist on
/// purpose: an allowlist silently drops a citation into a file type nobody
/// thought of, this can only produce visible noise.
const NON_FILE_EXTENSIONS: &[&str] = &[
    "com", "net", "org", "io", "dev", "local", "onion", "i2p", "de",
];

/// Directories never walked, for either the corpus or the resolution index.
/// `docs/book` would shadow the doc sources; `target` holds build output and
/// vendored source copies.
const SKIP_DIRS: &[&str] = &[".git", "target", "book", "node_modules"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

fn walk(dir: &Path, skip: &[&str], out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if skip.contains(&name.as_ref()) {
                continue;
            }
            walk(&path, skip, out);
        } else {
            out.push(path);
        }
    }
}

/// Which corpus a citation was found in, and therefore how it is spelled.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Corpus {
    /// Markdown prose: a citation is always inside backticks.
    Book,
    /// Rust source: citations live in comments *and* in assertion messages
    /// (`"... (Transport.py:2176)"`), and are usually not backticked. The
    /// pattern therefore matches bare, which is why the path shape has to
    /// carry the whole burden of not matching ordinary code.
    Source,
}

/// One `path:line[-line][,line[-line]…]` citation found in a file.
struct Citation {
    doc: PathBuf,
    doc_line: usize,
    /// Byte offset of the citation in its file. `doc_line` locates it for a
    /// reader; this locates it for the fixer, which has to rewrite one
    /// citation on a line that may carry several.
    offset: usize,
    raw: String,
    path: String,
    /// Inclusive line spans: `329` → [(329,329)], `155-199,204` →
    /// [(155,199),(204,204)].
    spans: Vec<(usize, usize)>,
    /// The backticked identifier attached to the citation, if it names
    /// what it points at. See [`attached_ident`].
    ident: Option<String>,
}

/// Directories whose files carry no extension, admitted by their directory.
///
/// The extension requirement below is load-bearing, so extensionless files
/// cannot simply be let in: dropping it would match `127.0.0.1:4242`, Python
/// slices, and any prose word followed by a number, and a guard with false
/// positives gets switched off (`docs/src/concepts/checks-and-citations.md`).
/// The hook scripts have neither an extension nor a name the pattern knows,
/// so until Codeberg #213 the guard's coverage of them was exactly zero: the
/// best-written possible citation into a hook was silently skipped while the
/// identical citation one file over resolved.
///
/// Admitting them by directory keeps the pattern as tight as it was —
/// `.githooks/` matches nothing else in the corpus, where a bare
/// extensionless token would match ordinary prose. It fails closed like
/// `NON_FILE_EXTENSIONS` beside it: a new extensionless file outside these
/// directories is uncovered rather than falsely flagged.
const EXTENSIONLESS_DIRS: &[&str] = &[".githooks"];

/// A path with a letters-only extension (or the extensionless `Justfile`, or
/// a file under an [`EXTENSIONLESS_DIRS`] directory) followed by `:` and a
/// line spec. The extension must be alphabetic so `127.0.0.1:4242` and Python
/// slices like `packed[:16]` do not match.
///
/// `word` is the left-hand boundary, and it sits *inside* the alternation
/// rather than in front of it. `Corpus::Source` matches bare, so a longer
/// word ending in a cited filename must not match at the inner offset — but
/// a `.githooks/` citation begins with a `.`, and `\b` before that only
/// matches when the preceding character is a word character, which after a
/// space or at the start of a comment it is not.
fn path_pattern(corpus: Corpus) -> String {
    let word = match corpus {
        Corpus::Source => r"\b",
        Corpus::Book => "",
    };
    let mut alts = vec![
        format!(r"{word}[A-Za-z0-9_][A-Za-z0-9_./-]*\.[A-Za-z]+"),
        format!("{word}Justfile"),
    ];
    alts.extend(
        EXTENSIONLESS_DIRS
            .iter()
            .map(|dir| format!(r"{}/[A-Za-z0-9_-]+", regex::escape(dir))),
    );
    format!("({})", alts.join("|"))
}

const SPEC_PATTERN: &str = r"(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)";

fn cite_regex(corpus: Corpus) -> Regex {
    let body = format!("{}:{SPEC_PATTERN}", path_pattern(corpus));
    match corpus {
        Corpus::Book => Regex::new(&format!("`{body}`")).unwrap(),
        // The right side is anchored by the line spec; the left by the `\b`
        // inside `path_pattern`.
        Corpus::Source => Regex::new(&body).unwrap(),
    }
}

/// The two spellings that attach a name to the citation that follows:
/// ``ident` (` and `` `ident`, ``. Whitespace (including line breaks) may
/// sit between; nothing else may. A trailing `()` (function spelling) is
/// stripped.
fn ident_regexes() -> [Regex; 4] {
    [
        Regex::new(r"`([A-Za-z0-9_:.]+)(?:\(\))?`\s*\(\s*$").unwrap(),
        Regex::new(r"`([A-Za-z0-9_:.]+)(?:\(\))?`\s*,\s*$").unwrap(),
        // A recipe is named `just <recipe>`, never by the bare token: the
        // corpus writes ``just standard` (`Justfile:<line>`)` -- spelled
        // without a line here, because this file is scanned as corpus and a
        // real one would be read as a citation. Hyphens are in because
        // recipe names use them and Rust identifiers do not.
        Regex::new(r"`just\s+([A-Za-z0-9_-]+)`\s*\(\s*$").unwrap(),
        Regex::new(r"`just\s+([A-Za-z0-9_-]+)`\s*,\s*$").unwrap(),
    ]
}

/// A backticked token that is itself a citation: `Destination.py:322`,
/// `Justfile:1167`. Tables list these next to each other, so without this
/// the second citation of a row would take the first as its subject.
fn citation_shaped() -> Regex {
    Regex::new(r"(?:\.[A-Za-z]+|^Justfile):\d").unwrap()
}

/// Whether a Justfile line *declares* the recipe `name`.
///
/// A plain substring search is the wrong test for a Justfile and produces a
/// false green, which is how the `run-status-parity.sh` citation survived:
/// it named Justfile line 857, the comment above 857 read "failed `just
/// standard`", so `standard` occurred within `WINDOW` of the cited line
/// while the recipe itself sat 140 lines further down. A recipe header is the only line that can be a definition — it
/// starts at column 0 (bodies are indented, comments start with `#`) and
/// the name is followed by its parameters and a colon.
fn defines_recipe(line: &str, name: &str) -> bool {
    let Some(rest) = line.strip_prefix(name) else {
        return false;
    };
    if !rest.contains(':') {
        return false;
    }
    let head = rest.split(':').next().unwrap_or_default();
    // `standard:` -> ""; `flash board:` -> " board"; `standard-extra:` ->
    // "-extra", a different recipe whose name merely starts with this one.
    head.is_empty() || head.starts_with(char::is_whitespace)
}

/// Whether `token` can be the name of a code item.
///
/// Fails closed, on both counts a real corpus supplies: a token that is
/// itself a citation, and one whose last segment carries no letter — a
/// `1209:0001` USB VID:PID, a `4.2:1` ratio. Neither can be searched for
/// as an identifier, and reading either as one would count a citation as
/// drift-checked while checking nothing.
fn looks_like_identifier(token: &str, citation_shaped: &Regex) -> bool {
    !citation_shaped.is_match(token)
        && token
            .rsplit([':', '.'])
            .next()
            .is_some_and(|seg| seg.chars().any(|c| c.is_ascii_alphabetic()))
}

/// A backticked code span, and the item a signature inside one declares.
///
/// `fn` rather than any identifier in the span: a signature names exactly
/// one item, so there is nothing to choose between. The `[(<]` is what
/// separates a declaration from prose about one — `fn` followed by a word
/// and then a paren or a generic list is a signature, `the fn above` is
/// not.
fn signature_regexes() -> [Regex; 2] {
    [
        Regex::new(r"`([^`\n]+)`").unwrap(),
        Regex::new(r"(?:^|[^A-Za-z0-9_])fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[(<]").unwrap(),
    ]
}

/// Everything needed to decide what a citation names, built once per scan.
struct Naming {
    idents: [Regex; 4],
    citation_shaped: Regex,
    /// `[0]` finds the backticked spans of a line, `[1]` the `fn` name
    /// inside one.
    signature: [Regex; 2],
    /// The corpus's own citation shape, used to reject a signature that
    /// has a citation of its own between it and this one.
    cite: Regex,
}

impl Naming {
    fn new(corpus: Corpus) -> Self {
        Naming {
            idents: ident_regexes(),
            citation_shaped: citation_shaped(),
            signature: signature_regexes(),
            cite: cite_regex(corpus),
        }
    }
}

/// The identifier the citation starting at the end of `before` names, if
/// any.
fn attached_ident(before: &str, n: &Naming) -> Option<String> {
    n.idents
        .iter()
        .find_map(|re| re.captures(before))
        .map(|c| c[1].to_string())
        .filter(|token| looks_like_identifier(token, &n.citation_shaped))
        // A `fn` name is an identifier by construction, so it needs no
        // filtering of its own.
        .or_else(|| signature_ident(before, n))
}

/// The item a table row names by spelling out its signature.
///
/// The two spellings above require the name to sit immediately before the
/// citation, which a reference table does not write: it writes
///
/// ```text
/// | `fn has_path(&self, dest_hash: &DestinationHash) -> bool` — `driver/mod.rs:NNNN` |
/// ```
///
/// (the line number stood in for here, because this file is itself in the
/// corpus it guards and a real one would be scanned as a citation)
///
/// and the name is inside a signature several words away. Those rows are
/// the densest citations in the book — 113 of them in
/// `docs/src/developer/rust-api-spec.md` alone — and every one of them
/// names its subject as plainly as a citation can. Reading none of them
/// left a 17-row method table 1035 lines out of date under a green guard
/// (Codeberg #307).
///
/// A row rather than a cell, because the corpus writes both `| `fn x()` —
/// `f.rs:N` |` and `| `Clock` | `fn now_ms()` | `f.rs:N` |` (again standing
/// in for the line number), and a cell boundary between a signature and the
/// citation of that signature is a typesetting choice. What keeps the pairing unambiguous is instead that
/// a citation between the signature and this one takes the signature for
/// itself: the row's second citation is then bare, as it was before.
fn signature_ident(before: &str, n: &Naming) -> Option<String> {
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    // Doc comments carry markdown tables too, so the `|` may be behind a
    // comment marker.
    let cell = line.trim_start();
    let cell = cell
        .strip_prefix("//!")
        .or_else(|| cell.strip_prefix("///"))
        .or_else(|| cell.strip_prefix("//"))
        .unwrap_or(cell);
    if !cell.trim_start().starts_with('|') {
        return None;
    }
    let (end, name) = n.signature[0]
        .captures_iter(line)
        .filter_map(|c| {
            let span = c.get(1)?;
            let name = n.signature[1].captures(span.as_str())?[1].to_string();
            Some((c.get(0)?.end(), name))
        })
        .last()?;
    (!n.cite.is_match(&line[end..])).then_some(name)
}

/// Scheme-relative or absolute URLs contain `host.tld` shapes that the path
/// pattern would otherwise accept. Only reachable in `Corpus::Source`, where
/// citations are not backticked.
fn inside_url(text: &str, start: usize) -> bool {
    let before = &text[..start];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = &before[line_start..];
    match line.rfind("//") {
        // A URL has no whitespace between `//` and the match.
        Some(i) => !line[i..].contains(char::is_whitespace) && line[..i].ends_with(':'),
        None => false,
    }
}

fn scan(root: &Path, files: &[PathBuf], corpus: Corpus) -> Vec<Citation> {
    let cite_re = cite_regex(corpus);
    let naming = Naming::new(corpus);
    let mut citations = Vec::new();
    for file in files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for m in cite_re.captures_iter(&text) {
            let whole = m.get(0).unwrap();
            if corpus == Corpus::Source {
                if inside_url(&text, whole.start()) {
                    continue;
                }
                let ext = m[1].rsplit('.').next().unwrap_or_default();
                if NON_FILE_EXTENSIONS.contains(&ext) {
                    continue;
                }
            }
            let spans = m[2]
                .split(',')
                .map(|part| match part.split_once('-') {
                    Some((a, b)) => (a.parse().unwrap(), b.parse().unwrap()),
                    None => {
                        let n = part.parse().unwrap();
                        (n, n)
                    }
                })
                .collect();
            citations.push(Citation {
                doc: file.strip_prefix(root).unwrap_or(file).to_path_buf(),
                doc_line: text[..whole.start()].matches('\n').count() + 1,
                offset: whole.start(),
                raw: whole.as_str().to_string(),
                path: m[1].to_string(),
                spans,
                ident: attached_ident(&text[..whole.start()], &naming),
            });
        }
    }
    citations
}

/// Every `docs/src/**/*.md` citation.
fn book_citations(root: &Path) -> Vec<Citation> {
    let mut mds = Vec::new();
    walk(&root.join("docs/src"), SKIP_DIRS, &mut mds);
    mds.retain(|p| p.extension().is_some_and(|e| e == "md"));
    mds.sort();
    assert!(
        !mds.is_empty(),
        "no markdown files under docs/src -- wrong repo root?"
    );
    scan(root, &mds, Corpus::Book)
}

/// Every `*.rs` citation in the three cited crates.
fn source_citations(root: &Path, crates: &[&str]) -> Vec<Citation> {
    let mut rs = Vec::new();
    for krate in crates {
        walk(&root.join(krate), SKIP_DIRS, &mut rs);
    }
    rs.retain(|p| p.extension().is_some_and(|e| e == "rs"));
    // The canary fixtures carry deliberately drifted citations; they are
    // the guard's own input, not part of the corpus it guards.
    rs.retain(|p| !p.components().any(|c| c.as_os_str() == "citation_canary"));
    rs.sort();
    assert!(
        !rs.is_empty(),
        "no Rust sources under {crates:?} -- wrong repo root?"
    );
    scan(root, &rs, Corpus::Source)
}

/// Every citation in the gate scripts and in the `Justfile`.
///
/// Until 2026-09-23 the corpus was the book plus four crates of Rust, so a
/// citation written in a shell script was not checked at all --
/// `scripts/run-status-parity.sh` pointed at Justfile line 857 for a recipe
/// that lives at 997, and 857 is a live line (a comment), so nothing
/// anywhere said so. The
/// scripts spell citations bare, in `#` comments, exactly like Rust
/// comments do, so they are scanned as `Corpus::Source`.
fn script_citations(root: &Path) -> Vec<Citation> {
    let mut files = Vec::new();
    walk(&root.join("scripts"), SKIP_DIRS, &mut files);
    files.retain(|p| p.extension().is_some_and(|e| e == "sh"));
    files.push(root.join("Justfile"));
    files.sort();
    assert!(
        files.len() > 1,
        "no shell scripts under scripts/ -- wrong repo root?"
    );
    scan(root, &files, Corpus::Source)
}

/// Distance in lines from `line` to the nearest edge of `span` (0 when
/// inside).
fn span_distance(line: usize, span: (usize, usize)) -> usize {
    if line < span.0 {
        span.0 - line
    } else {
        line.saturating_sub(span.1)
    }
}

/// Whether the cited span lies inside the block the identifier at `hit`
/// introduces. Both are 1-based line numbers.
///
/// The second thing a citation means. `` `remember_ticket`,
/// `LXMRouter.py:1102-1105` `` points at the item itself and the
/// adjacency window sees it; `` `Transport.request_path`,
/// `Transport.py:2786-2787` `` names the *enclosing* function and points at
/// two statements 15 lines into its body, which adjacency cannot see and
/// which is just as much "the citation names what it points at". Both
/// spellings are in the corpus in roughly equal numbers, so a rule that
/// only understood the first would report every instance of the second —
/// and a guard with false positives gets switched off.
///
/// Indentation rather than syntax, so this needs no parser and holds for
/// Rust, Python, C and the Arduino sources alike: the line that names an
/// item is less indented than every line of its body. Blank lines carry no
/// indentation of their own and are skipped.
///
/// What it gives up: an identifier that merely occurs on some line above
/// the span, with the span nested deeper, satisfies this without being the
/// enclosing item — a `let` binding above a block, say. So this is a
/// weaker check than adjacency, not a stronger one. It is still the
/// difference between "the citation is somewhere in the named item" and
/// "the citation is somewhere in the file", which is what the existence
/// check already was.
fn encloses(lines: &[&str], hit: usize, span_start: usize) -> bool {
    if hit >= span_start || hit == 0 || span_start > lines.len() {
        return false;
    }
    let indent = |l: &&str| l.len() - l.trim_start().len();
    let base = indent(&lines[hit - 1]);
    lines[hit..span_start]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .all(|l| indent(l) > base)
}

#[derive(Debug, PartialEq, Eq)]
enum FailureKind {
    /// Nothing in the tree matches the cited path.
    Missing,
    /// The path names a `reference/` submodule that is not checked out.
    /// Not the citation's fault, and not fixable by editing it.
    SubmoduleAbsent,
    /// The file is there and long enough, but the cited identifier is not
    /// near the cited line.
    Drift,
    /// The citation's own line spec runs backwards (`a-b` with `a > b`).
    /// Nothing about the tree can make this right, so it is refused where
    /// it is written rather than resolved.
    Inverted,
}

struct Failure {
    kind: FailureKind,
    message: String,
}

/// A resolved identifier citation and how many lines its identifier sits
/// from the cited span. Zero is exact; anything else is `WINDOW` budget
/// already spent at landing time, and a citation that lands at the edge
/// reddens on the next unrelated insertion above it (which is how 33
/// citations born at +7/+8 in one commit all tipped over when a later
/// commit added 4 lines).
struct Offset {
    where_: String,
    dist: usize,
    nearest: usize,
}

#[derive(Default)]
struct Counts {
    with_ident: usize,
    bare: usize,
    external: usize,
    /// One entry per identifier citation that resolved by adjacency.
    /// Citations that resolve only by enclosure carry no meaningful
    /// offset (they point at statements inside the named item) and are
    /// not listed.
    offsets: Vec<Offset>,
}

impl Counts {
    fn total(&self) -> usize {
        self.with_ident + self.bare + self.external
    }
}

/// The offset histogram for a corpus, plus every non-exact citation by
/// name. Run with `--nocapture` to see it. A green guard hides how much
/// drift budget is already spent; this is what makes it visible.
fn report_offsets(label: &str, counts: &Counts) {
    let mut hist: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for o in &counts.offsets {
        *hist.entry(o.dist).or_default() += 1;
    }
    let buckets: Vec<String> = hist.iter().map(|(d, n)| format!("+{d}:{n}")).collect();
    println!(
        "{label} identifier-citation offsets (lines from cited span to nearest \
         identifier, +0 = exact, tolerance {WINDOW}): {}",
        buckets.join(" ")
    );
    for o in counts.offsets.iter().filter(|o| o.dist > 0) {
        println!(
            "  +{}: {} (identifier at line {})",
            o.dist, o.where_, o.nearest
        );
    }
}

/// The submodules under `<root>/reference` that have no working tree.
fn absent_submodules(root: &Path) -> BTreeSet<&'static str> {
    SUBMODULES
        .iter()
        .copied()
        .filter(|s| {
            fs::read_dir(root.join("reference").join(s))
                .map(|mut d| d.next().is_none())
                .unwrap_or(true)
        })
        .collect()
}

/// The single checking core, shared by the book guard, the source guard and
/// the canary. `root` is both the resolution root and the prefix stripped
/// from reported paths.
fn check(root: &Path, citations: &[Citation]) -> (Counts, Vec<Failure>) {
    let mut files = Vec::new();
    walk(root, SKIP_DIRS, &mut files);
    let rel_files: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    // Bare filenames like `transport.rs:257` resolve by suffix.
    let resolve = |cited: &str| -> Vec<&str> {
        let suffix = format!("/{cited}");
        rel_files
            .iter()
            .filter(|f| *f == cited || f.ends_with(&suffix))
            .map(String::as_str)
            .collect()
    };

    let absent = absent_submodules(root);
    // A citation spelled `reference/LXMF/...` names its submodule outright.
    // A bare `LXMRouter.py:123` does not, so an absent submodule can only be
    // offered as a caveat on the missing-file failure.
    let named_submodule = |path: &str| -> Option<&'static str> {
        let rest = path.strip_prefix("reference/")?;
        SUBMODULES
            .iter()
            .copied()
            .find(|s| rest.strip_prefix(s).is_some_and(|r| r.starts_with('/')))
    };

    let mut counts = Counts::default();
    let mut failures = Vec::new();

    for c in citations {
        if EXTERNAL_PREFIXES.iter().any(|p| c.path.starts_with(p)) {
            counts.external += 1;
            continue;
        }
        match c.ident {
            Some(_) => counts.with_ident += 1,
            None => counts.bare += 1,
        }
        let where_ = format!("{}:{}: {}", c.doc.display(), c.doc_line, c.raw);

        // Refused on the way in, before anything is resolved: a backwards
        // range cannot be repaired later. `repaired()` declines to emit one,
        // so once the anchors under an inverted citation move it is reported
        // as undecidable drift with no suggestion, forever. Three of them
        // reached `docs/src/architecture-broadcast-python-parity.md` that
        // way. The endpoints are the author's to re-read, not ours to swap:
        // two of those three named code that had moved as well.
        if let Some(&(a, b)) = c.spans.iter().find(|&&(a, b)| a > b) {
            failures.push(Failure {
                kind: FailureKind::Inverted,
                message: format!(
                    "{where_}\n    the line spec runs backwards ({a}-{b}): a range ends \
                     after it starts.\n    Re-read the span and write the endpoints in \
                     order -- do not simply swap them, an inverted range is usually a \
                     citation nobody has read since the code under it moved."
                ),
            });
            continue;
        }

        if let Some(sub) = named_submodule(&c.path) {
            if absent.contains(sub) {
                failures.push(Failure {
                    kind: FailureKind::SubmoduleAbsent,
                    message: format!(
                        "{where_}\n    reference/{sub} is not checked out, so this citation \
                         cannot be verified.\n    This is NOT a drifted citation -- do not edit \
                         it. Check the reference out:\n        git submodule update --init \
                         reference/{sub}\n    (that the checkout matches the gitlink is a \
                         separate check: scripts/check-submodule-pins.sh)"
                    ),
                });
                continue;
            }
        }

        let candidates = resolve(&c.path);
        if candidates.is_empty() {
            let hint = if absent.is_empty() {
                String::new()
            } else {
                format!(
                    "\n    (reference/{} not checked out -- if this cites one of them, it is \
                     absent rather than drifted; `git submodule update --init` before trusting \
                     this failure)",
                    absent
                        .iter()
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", reference/")
                )
            };
            failures.push(Failure {
                kind: FailureKind::Missing,
                message: format!(
                    "{where_}\n    no file matching `{}` in the repo -- deleted or renamed?{hint}",
                    c.path
                ),
            });
            continue;
        }

        // A citation passes if any candidate file satisfies every check;
        // bare filenames can be ambiguous (two `constants.rs` exist) and
        // the prose, not the path, disambiguates.
        let max_line = c.spans.iter().map(|s| s.1).max().unwrap();
        let mut candidate_notes = Vec::new();
        let mut passed = false;
        for cand in &candidates {
            let text = fs::read_to_string(root.join(cand)).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() < max_line {
                candidate_notes.push(format!(
                    "    {cand} has only {} lines (cited: {max_line})",
                    lines.len()
                ));
                continue;
            }
            let Some(ident) = &c.ident else {
                passed = true;
                break;
            };
            // `Type::method` / `module.attr` cite the item; the source
            // line contains the last segment.
            let needle = ident.rsplit(&[':', '.'][..]).next().unwrap();
            // A Justfile has one kind of definition and it is not a
            // substring: see [`defines_recipe`].
            let is_justfile = Path::new(cand).file_name().is_some_and(|f| f == "Justfile");
            let hits: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| {
                    if is_justfile {
                        defines_recipe(l, needle)
                    } else {
                        l.contains(needle)
                    }
                })
                .map(|(i, _)| i + 1)
                .collect();
            let resolved = hits.iter().any(|&h| {
                c.spans
                    .iter()
                    .any(|&span| span_distance(h, span) <= WINDOW || encloses(&lines, h, span.0))
            });
            if resolved {
                // `resolved` implies at least one hit, and every citation
                // carries at least one span.
                let (nearest, dist) = hits
                    .iter()
                    .map(|&h| {
                        (
                            h,
                            c.spans.iter().map(|&s| span_distance(h, s)).min().unwrap(),
                        )
                    })
                    .min_by_key(|&(_, d)| d)
                    .unwrap();
                if dist <= WINDOW {
                    counts.offsets.push(Offset {
                        where_: where_.clone(),
                        dist,
                        nearest,
                    });
                }
                passed = true;
                break;
            }
            let cited_first = c.spans[0].0;
            let nearest = hits
                .iter()
                .min_by_key(|&&h| c.spans.iter().map(|&s| span_distance(h, s)).min().unwrap());
            let what = if is_justfile {
                format!("recipe `{needle}`")
            } else {
                format!("`{needle}`")
            };
            candidate_notes.push(match nearest {
                Some(&n) => format!(
                    "    {what} not within {WINDOW} lines of the cited span in {cand}\n    cited line {cited_first}: {}\n    nearest {what}: line {n}: {}",
                    lines[cited_first - 1].trim(),
                    lines[n - 1].trim()
                ),
                None => format!("    {what} does not occur anywhere in {cand}"),
            });
        }
        if !passed {
            failures.push(Failure {
                kind: FailureKind::Drift,
                message: format!("{where_}\n{}", candidate_notes.join("\n")),
            });
        }
    }

    (counts, failures)
}

// --- figure attribution (Codeberg #200) ----------------------------------

/// A document path under `docs/` ending in `.md`. Any `:line` suffix stops
/// the match on its own, so the same reference is seen whether or not it
/// carries one.
const DOC_PATH_PATTERN: &str = r"[A-Za-z0-9_.-]*docs/[A-Za-z0-9_./-]*\.md";

/// A decimal figure. Boundaries are applied separately in `figures_in`,
/// because the `regex` crate has no lookaround.
const FIGURE_PATTERN: &str = r"\d+\.\d+";

/// One `///`/`//!` paragraph: the contiguous doc-comment lines between two
/// blank doc-comment lines (or between a blank one and the end of the run).
struct Paragraph {
    file: PathBuf,
    /// Line number of the paragraph's first line, in the source file.
    first_line: usize,
    text: String,
}

/// Every doc-comment paragraph in `files`.
///
/// Paragraph, not sentence, and that is the load-bearing choice. The #200
/// defect attributed its figure across a sentence boundary — "The number
/// comes off `docs/…`" in one sentence, "The failure mode it names is
/// 126.6 ms" two sentences later — so a sentence-scoped trigger would have
/// sailed past the case it exists for. Paragraph scope also avoids having
/// to segment sentences at all, which in this corpus means deciding whether
/// the `.` in `126.6`, in `core-lock-budget.md` and in `e.g.` ends one.
fn doc_paragraphs(root: &Path, files: &[PathBuf]) -> Vec<Paragraph> {
    let mut out = Vec::new();
    for file in files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let rel = file.strip_prefix(root).unwrap_or(file).to_path_buf();
        let mut acc: Vec<&str> = Vec::new();
        let mut first = 0usize;
        let flush = |acc: &mut Vec<&str>, first: usize, out: &mut Vec<Paragraph>| {
            if !acc.is_empty() {
                out.push(Paragraph {
                    file: rel.clone(),
                    first_line: first,
                    text: acc.join(" "),
                });
                acc.clear();
            }
        };
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            let body = trimmed
                .strip_prefix("///")
                .or_else(|| trimmed.strip_prefix("//!"))
                .map(str::trim);
            match body {
                Some("") | None => flush(&mut acc, first, &mut out),
                Some(b) => {
                    if acc.is_empty() {
                        first = i + 1;
                    }
                    acc.push(b);
                }
            }
        }
        flush(&mut acc, first, &mut out);
    }
    out
}

/// Whether the byte range `[start, end)` of `text` is a standalone number:
/// not glued to another digit or to a further `.`.
///
/// This is what keeps `0.8.0`, `1.3.4` and `127.0.0.1` out. Each yields
/// `0.8` / `1.3` / `127.0` from the pattern and each is rejected here for
/// the `.` that follows.
fn is_standalone(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let glued = |c: Option<char>| c.is_some_and(|c| c.is_ascii_digit() || c == '.');
    !glued(before) && !glued(after)
}

/// The standalone decimal figures in `text`, in order, deduplicated.
fn figures_in(re: &Regex, text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    re.find_iter(text)
        .filter(|m| is_standalone(text, m.start(), m.end()))
        .map(|m| m.as_str().to_string())
        .filter(|f| seen.insert(f.clone()))
        .collect()
}

/// Whether `figure` occurs in `text` as a standalone number.
fn document_names_figure(re: &Regex, text: &str, figure: &str) -> bool {
    re.find_iter(text)
        .any(|m| m.as_str() == figure && is_standalone(text, m.start(), m.end()))
}

/// Every decimal figure a doc comment attributes to a named document must
/// occur in that document.
///
/// Returns `(paragraphs triggered, figures checked, failures)`.
///
/// # Where the trigger is drawn, and what that gives up
///
/// A guard with false positives gets switched off, and a switched-off guard
/// is worse than none, so this is narrow on purpose and the cost is stated
/// rather than hidden.
///
/// It fires only where **a paragraph names a document under `docs/` and
/// quotes a decimal figure**. Decimal, because that is what separates a
/// measurement somebody took from a number somebody derived in the same
/// breath. In the paragraph the #200 defect lived in, "126.6 ms", "3.2 ms"
/// and "0.8 ms" are the page's figures, while "5 ms" is the constant being
/// defined, "~25x" is arithmetic done in the comment, and "141 ms", "8" and
/// "256 KiB" are the page's too but round. Checking every number would have
/// reported the constant's own value and a ratio as unattributed on the
/// tree as it stood — three false positives against one true one, on the
/// very comment this exists for.
///
/// What that gives up, in order of how much it costs:
///
/// - **Integer figures.** "the page names 141 ms" is unchecked. This is the
///   real gap, and it is not small: a wrong round number is as believable
///   as a wrong precise one.
/// - **Attribution across paragraphs.** A figure a paragraph below the one
///   naming the document is unchecked. Doc comments break paragraphs at
///   headings, so a `# Where the number comes from` section that names the
///   page in its first paragraph and the figure in its second is missed.
/// - **Ordinary `//` comments and prose in `docs/src/**`.** Only `///` and
///   `//!` are read, in the four crates `SOURCE_CRATES` names.
/// - **Which figure means what.** A paragraph naming two documents passes
///   if the figure is in either, and a figure that appears in the document
///   in an unrelated sentence passes. This checks that the number is on the
///   page, not that the page says what the comment says it says.
/// - **Non-numeric attributions.** "the page forbids X" is prose (kind 3 in
///   the module header) and stays prose.
fn figure_attributions(root: &Path, crates: &[&str]) -> (usize, usize, Vec<Failure>) {
    let mut rs = Vec::new();
    for krate in crates {
        walk(&root.join(krate), SKIP_DIRS, &mut rs);
    }
    rs.retain(|p| p.extension().is_some_and(|e| e == "rs"));
    rs.retain(|p| !p.components().any(|c| c.as_os_str() == "citation_canary"));
    rs.sort();

    let doc_re = Regex::new(DOC_PATH_PATTERN).unwrap();
    let fig_re = Regex::new(FIGURE_PATTERN).unwrap();

    let mut index = Vec::new();
    walk(root, SKIP_DIRS, &mut index);
    let rel_index: Vec<String> = index
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let mut triggered = 0;
    let mut checked = 0;
    let mut failures = Vec::new();

    for para in doc_paragraphs(root, &rs) {
        let named: BTreeSet<String> = doc_re
            .find_iter(&para.text)
            .map(|m| m.as_str().to_string())
            .collect();
        if named.is_empty() {
            continue;
        }
        let figures = figures_in(&fig_re, &para.text);
        if figures.is_empty() {
            continue;
        }
        triggered += 1;

        let where_ = format!("{}:{}", para.file.display(), para.first_line);
        let mut bodies = Vec::new();
        let mut absent = Vec::new();
        for name in &named {
            let suffix = format!("/{name}");
            match rel_index
                .iter()
                .find(|f| *f == name || f.ends_with(&suffix))
            {
                Some(found) => bodies.push((
                    found.clone(),
                    fs::read_to_string(root.join(found)).unwrap_or_default(),
                )),
                None => absent.push(name.clone()),
            }
        }
        if !absent.is_empty() {
            // A doc comment attributing a figure to a document that is not
            // in the tree is the same defect one step further along: there
            // is nothing left to check the figure against.
            failures.push(Failure {
                kind: FailureKind::Missing,
                message: format!(
                    "{where_}\n    a figure is attributed to {}, which is not in the repo \
                     -- deleted or renamed?\n    figures in the paragraph: {}",
                    absent.join(", "),
                    figures.join(", ")
                ),
            });
            continue;
        }

        for figure in &figures {
            checked += 1;
            if bodies
                .iter()
                .any(|(_, body)| document_names_figure(&fig_re, body, figure))
            {
                continue;
            }
            let names: Vec<&str> = bodies.iter().map(|(n, _)| n.as_str()).collect();
            failures.push(Failure {
                kind: FailureKind::Drift,
                message: format!(
                    "{where_}\n    the figure {figure} is attributed to {}, which does not \
                     contain it.\n    Either the number is wrong, or it was never written down \
                     where the comment says it was --\n    a measurement that lives only in the \
                     comment claiming to quote it cannot be checked by anyone.\n    Put it on \
                     the page, or attribute it to where it actually is.\n    Paragraph: {}",
                    names.join(" or "),
                    para.text.chars().take(300).collect::<String>()
                ),
            });
        }
    }

    (triggered, checked, failures)
}

fn report(label: &str, failures: &[Failure]) {
    assert!(
        failures.is_empty(),
        "{} {label} citation(s) no longer point at what they claim.\n\
         Fix the citation, not the guard: each entry names the file and line, \
         the citation as written, and the nearest current match.\n\n{}",
        failures.len(),
        failures
            .iter()
            .map(|f| f.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}

// --- standing canary -----------------------------------------------------
//
// The concept page requires a permanent pair, not a one-time demonstration:
// a gate that stops matching -- a glob that no longer resolves, a parser that
// returns nothing -- is green forever, which is the defect the page exists to
// remove. The floor asserts below catch a parser that stops matching; this
// catches a checker that stops *failing*.
//
// The fixture is a miniature repo built in a tempdir and run through the same
// `scan` + `check` the real corpora use, so it exercises parse, resolve,
// window and classification end to end without polluting the corpus it
// guards. Both directions are asserted: the drifted, absent and missing
// citations must be reported, and the correct one must not.

/// Fixture bodies live in `tests/citation_canary/*.in`, not inline: a citation
/// spelled out in this file would be scanned as part of the corpus this file
/// guards. The fixture puts its subject at line `CANARY_SUBJECT_LINE` and pads
/// to 60 lines, so the citation to `CANARY_DRIFT_LINE` is inside the file but
/// far outside `WINDOW`. `tests/citation_canary/README.md` says what each of
/// the fixture citations is for.
const CANARY_TARGET: &str = include_str!("citation_canary/canary_target.rs.in");
const CANARY_CITATIONS: &str = include_str!("citation_canary/canary_citations.rs.in");
const CANARY_FIGURES: &str = include_str!("citation_canary/canary_figures.rs.in");
const CANARY_BUDGET: &str = include_str!("citation_canary/canary_budget.md.in");
const CANARY_SUBJECT_LINE: usize = 3;
const CANARY_DRIFT_LINE: usize = 50;

fn write_canary_fixture(root: &Path) {
    let src = root.join("leviculum-core/src/citation_canary");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("canary_target.rs"), CANARY_TARGET).unwrap();
    fs::write(src.join("canary_citations.rs"), CANARY_CITATIONS).unwrap();
}

/// The figure-attribution fixture. A separate tree from the one above: the
/// citation canary excludes `citation_canary` paths from its scan, and this
/// one needs its Rust file *inside* the corpus.
fn write_figure_canary_fixture(root: &Path) {
    let src = root.join("leviculum-core/src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("canary_figures.rs"), CANARY_FIGURES).unwrap();
    let docs = root.join("docs/src/concepts");
    fs::create_dir_all(&docs).unwrap();
    fs::write(docs.join("canary_budget.md"), CANARY_BUDGET).unwrap();
}

/// Both directions, on the same fixture: the unsupported figure and the
/// missing page must be reported, and the supported figure, the version
/// string and the two integers in the same paragraph must not.
///
/// A one-time demonstration is not enough. A trigger that stops matching --
/// a paragraph splitter that returns nothing, a boundary rule that rejects
/// every figure -- reports zero findings forever, which reads exactly like
/// a clean tree. It is the shape this whole page exists to remove, and the
/// #200 case itself was fixed hours before the guard was written, so the
/// real corpus cannot supply the failing side.
fn run_figure_canary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_figure_canary_fixture(root);

    let (triggered, checked, failures) = figure_attributions(root, &["leviculum-core"]);
    // Two of the fixture's three paragraphs, exactly. The third names the
    // page and quotes only the version string `0.8.0`, which the boundary
    // rule must not read as the figure `0.8`. So this number is pinned in
    // both directions at once: 1 or 0 means the trigger stopped matching and
    // a green run means nothing, 3 means the boundary rule broke and every
    // version string in the tree is about to be reported as a figure.
    assert_eq!(
        triggered, 2,
        "CANARY: {triggered} of the fixture's 3 paragraphs triggered, expected 2."
    );
    // 3.2 and 126.6. The third paragraph's 1.5 is deliberately not among
    // them: its page is absent, and there is nothing to check a figure
    // against.
    assert_eq!(
        checked, 2,
        "CANARY: {checked} figures were checked, expected 3.2 and 126.6. \
         Figure extraction has stopped working."
    );

    let msgs: Vec<&str> = failures.iter().map(|f| f.message.as_str()).collect();
    let joined = msgs.join("\n\n");
    assert!(
        failures
            .iter()
            .any(|f| f.kind == FailureKind::Drift && f.message.contains("figure 126.6")),
        "CANARY: a figure attributed to a page that does not contain it was NOT \
         reported. This is the defect the check exists for.\n{joined}"
    );
    assert!(
        failures
            .iter()
            .any(|f| f.kind == FailureKind::Missing && f.message.contains("canary_gone.md")),
        "CANARY: an attribution to a document absent from the tree was not \
         reported; a renamed page would switch the check off silently.\n{joined}"
    );
    // Everything below is the false-positive side. A guard that reports these
    // gets switched off, and a switched-off guard is worse than none.
    for quiet in ["figure 3.2", "figure 0.8", "figure 5", "figure 25"] {
        assert!(
            !joined.contains(quiet),
            "CANARY: `{quiet}` was reported. 3.2 is on the page, 0.8 is half of \
             the version string 0.8.0, and 5 and 25 are integers the comment \
             derives rather than quotes.\n{joined}"
        );
    }
    assert_eq!(
        failures.len(),
        2,
        "CANARY: expected exactly the drift and the missing page; got {}:\n{joined}",
        failures.len()
    );
}

fn run_canary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_canary_fixture(root);

    let citations = scan(
        root,
        &{
            let mut v = Vec::new();
            walk(&root.join("leviculum-core"), SKIP_DIRS, &mut v);
            v.retain(|p| p.extension().is_some_and(|e| e == "rs"));
            v.sort();
            v
        },
        Corpus::Source,
    );
    assert_eq!(
        citations.len(),
        12,
        "CANARY: the parser found {} of 12 fixture citations. It has stopped \
         matching; every green run since it broke means nothing.",
        citations.len()
    );

    let (counts, failures) = check(root, &citations);
    // Nine of the twelve name what they point at: four in the paren
    // spelling, two in the comma spelling, three in the table spelling. The
    // three that do not are the second citation of the not-an-identifier
    // line, whose leading backticked token is itself a citation, and the
    // second citation of the last table row, whose signature was already
    // claimed by the first -- pinned in both directions at once, because a
    // lower number means a spelling stopped being seen (and hundreds of real
    // citations silently fell back to an existence check) while a higher one
    // means a citation beside a citation is being read as its subject.
    assert_eq!(
        counts.with_ident, 9,
        "CANARY: identifier detection saw {} of 9, which silently changes \
         how much of the corpus is drift-checked.",
        counts.with_ident
    );

    let kinds: Vec<&FailureKind> = failures.iter().map(|f| &f.kind).collect();
    assert_eq!(
        kinds.len(),
        5,
        "CANARY: expected exactly 5 failures (three drifts, missing, absent \
         submodule); got {}:\n{}",
        kinds.len(),
        failures
            .iter()
            .map(|f| f.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    // Built rather than spelled out: a literal citation in this file would be
    // scanned as part of the corpus this file guards.
    let drifted = format!("canary_target.rs:{CANARY_DRIFT_LINE}");
    let correct = format!("canary_target.rs:{CANARY_SUBJECT_LINE}");
    // The drifted citations must be reported: this is the failure the guard
    // exists for, and the one that decays silently. Once per spelling --
    // a comma-form drift that goes unreported is the `lora.rs:1061` case
    // over again, and a table-form one is Codeberg #307 over again, which is
    // what admitting each spelling was for.
    let drifts: Vec<&Failure> = failures
        .iter()
        .filter(|f| f.kind == FailureKind::Drift && f.message.contains(&drifted))
        .collect();
    assert_eq!(
        drifts.len(),
        3,
        "CANARY: {} of the 3 deliberately drifted citations (paren spelling, \
         comma spelling, table spelling) were reported. The guard cannot see \
         the defect it exists to catch in one of the three forms the corpus \
         writes.",
        drifts.len()
    );
    // The correct ones must not be, or the guard is noise and gets disabled.
    // `: <raw>\n` is how `check` opens a failure message, so this matches the
    // citation as written and not a line number quoted inside a note.
    assert!(
        !failures
            .iter()
            .any(|f| f.message.contains(&format!(": {correct}\n"))),
        "CANARY: a correct citation was reported as broken:\n{}",
        failures
            .iter()
            .map(|f| f.message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    // Absent-submodule and drift must stay distinguishable: sending a reader
    // to `git submodule update --init` for a drifted citation, or to the
    // prose for an absent reference, is how the LXMF incident stayed open.
    let absent = failures
        .iter()
        .find(|f| f.kind == FailureKind::SubmoduleAbsent)
        .expect("CANARY: a citation into an unchecked-out submodule was not classified as absent");
    assert!(
        absent.message.contains("NOT a drifted citation"),
        "CANARY: the absent-submodule message no longer distinguishes itself \
         from a drift: {}",
        absent.message
    );
    assert!(
        failures
            .iter()
            .any(|f| f.kind == FailureKind::Missing && f.message.contains("canary_absent_file.rs")),
        "CANARY: a citation to a nonexistent file was not reported."
    );
}

#[test]
fn citation_guard_canary() {
    run_canary();
    run_figure_canary();
}

/// Codeberg #213: a citation into an extensionless hook under `.githooks/`
/// is seen, and nothing else became visible with it.
///
/// The guard's coverage of that directory used to be exactly zero — the
/// hooks have neither an extension nor a name the pattern knew, so the
/// best-written possible citation into one was skipped while the identical
/// citation one file over resolved. `post-commit` was then deleted with
/// seven references to it across five files and the guard ran green in the
/// same invocation.
///
/// Both directions are pinned, because the extension requirement that
/// caused the blind spot is also what keeps `127.0.0.1:4242` and Python
/// slices out, and a guard with false positives gets switched off.
#[test]
fn githook_citations_match_without_loosening_the_pattern() {
    let source = cite_regex(Corpus::Source);
    let book = cite_regex(Corpus::Book);

    // The leading dot is part of the captured path: the directory is spelled
    // `.githooks` on disk and `check` resolves the path as written, so a
    // capture that dropped it would resolve to nothing.
    //
    // The fixture citations are BUILT, like the canary's, and now for a
    // second reason on top of "a literal here is scanned as corpus": this
    // file is in the corpus, the anchor check reports a fixture line number
    // that has moved in the real hook, and `LEVICULUM_CITATION_FIX` then
    // rewrites the number this test asserts on. It did exactly that once.
    let subject = format!(
        "lints the pipelines, .githooks/pre-push:{}, before Tier 0",
        21
    );
    let caps = source
        .captures(&subject)
        .expect("a .githooks citation in a source comment must match");
    assert_eq!(&caps[1], ".githooks/pre-push");
    assert_eq!(&caps[2], "21");

    let subject = format!(
        "the commit-msg hook (`.githooks/commit-msg:{}`) does the same",
        5
    );
    let caps = book
        .captures(&subject)
        .expect("a .githooks citation in the book must match");
    assert_eq!(&caps[1], ".githooks/commit-msg");

    // A citation into a hook that has been deleted must now be *matched*, so
    // that `check` can report it missing — that is the whole point, and the
    // half no green run can demonstrate. Built rather than spelled out: a
    // literal here would be scanned as part of the corpus this file guards
    // and would fail the guard it is testing.
    let deleted = format!(".githooks/{}-commit:10", "post");
    assert!(
        source.is_match(&deleted),
        "a citation into a deleted hook must be matched so it can be reported"
    );

    // The false-positive side. Extensionless *tokens* stay out; only paths
    // under an allowlisted directory come in.
    for prose in [
        "pre-push:21",
        "post-commit:10",
        "commit-msg:5",
        "127.0.0.1:4242",
        "packed[:16]",
        // A path is a citation only with a line spec; that has not changed.
        ".githooks/pre-push",
    ] {
        assert!(
            !source.is_match(prose),
            "`{prose}` must not be read as a citation -- the pattern has been \
             loosened into matching prose"
        );
    }

    // End to end through the real `scan` + `check`, on a fixture repo: a
    // hook citation that resolves must stay quiet, and one into a hook that
    // is not there must be reported missing.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    fs::create_dir_all(root.join(".githooks")).unwrap();
    fs::write(
        root.join(".githooks/pre-push"),
        "#!/bin/bash\nset -e\njust fast\n",
    )
    .unwrap();
    let docs = root.join("docs/src");
    fs::create_dir_all(&docs).unwrap();
    let gone = format!(".githooks/{}-commit:1", "post");
    fs::write(
        docs.join("hooks.md"),
        format!(
            "Tier 0 runs from `.githooks/pre-push:{}`, and once from `{gone}`.\n",
            3
        ),
    )
    .unwrap();

    let citations = scan(root, &[docs.join("hooks.md")], Corpus::Book);
    assert_eq!(
        citations.len(),
        2,
        "the fixture's two hook citations were not both parsed"
    );
    let (_, failures) = check(root, &citations);
    let messages: Vec<&str> = failures.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(
        failures.len(),
        1,
        "expected exactly the citation into the absent hook: {}",
        messages.join("\n\n")
    );
    assert_eq!(failures[0].kind, FailureKind::Missing);
    assert!(
        failures[0].message.contains(&gone),
        "the reported failure names the wrong citation: {}",
        failures[0].message
    );
}

/// A line spec that runs backwards is refused where it is written.
///
/// The positive control for the check `check` does before it resolves
/// anything. Both directions on one fixture: the forwards range must stay
/// quiet, the backwards one must be reported, and neither depends on the
/// cited file being short -- the target here is long enough for both.
///
/// Why this is worth a check of its own rather than "the author will
/// notice": `repaired()` refuses to emit an inverted span, so once the
/// anchors under such a citation move, the drift report says only "not all
/// of this citation's endpoints could be placed" and offers nothing. The
/// citation then survives every gate until somebody reads it. Three did,
/// in one page, for months.
#[test]
fn an_inverted_line_spec_is_refused_where_it_is_written() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let target: String = (1..=60).map(|i| format!("// line {i}\n")).collect();
    fs::write(root.join("target.rs"), &target).unwrap();

    let docs = root.join("docs/src");
    fs::create_dir_all(&docs).unwrap();
    // Built, not spelled out: a literal citation here is scanned as part of
    // the corpus this file guards.
    fs::write(
        docs.join("ranges.md"),
        format!(
            "Forwards `target.rs:{}-{}` is fine; backwards `target.rs:{}-{}` is not.\n",
            10, 20, 20, 10
        ),
    )
    .unwrap();

    let citations = scan(root, &[docs.join("ranges.md")], Corpus::Book);
    assert_eq!(
        citations.len(),
        2,
        "the fixture's two range citations were not both parsed"
    );

    let (_, failures) = check(root, &citations);
    let messages: Vec<&str> = failures.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(
        failures.len(),
        1,
        "expected exactly the backwards range: {}",
        messages.join("\n\n")
    );
    assert_eq!(failures[0].kind, FailureKind::Inverted);
    assert!(
        failures[0].message.contains("20-10"),
        "the report names the wrong span: {}",
        failures[0].message
    );
}

/// A `Justfile:<n>` citation that names a recipe must land on the recipe's
/// definition, not on prose that mentions it.
///
/// This is the shape `scripts/run-status-parity.sh` carried: the cited line
/// was a comment, the line above it mentioned the recipe by name, and the
/// recipe sat 140 lines further down. Existence-checking passes it (the
/// line is live), and so does a substring search for the name (the mention
/// is one line away). Only a definition check sees it.
#[test]
fn a_justfile_citation_lands_on_the_recipe_and_not_on_a_mention_of_it() {
    // The predicate on its own, which is the whole difference: the mention
    // is not a definition, the header is, and a longer name that merely
    // starts with this one is not.
    assert!(!defines_recipe(
        "# the gate has failed `just standard` on every sha since.",
        "standard"
    ));
    assert!(!defines_recipe("    cargo test standard", "standard"));
    assert!(!defines_recipe("standard-extra: fast", "standard"));
    assert!(defines_recipe(
        "standard: fast test-ffi verify-packaging",
        "standard"
    ));
    assert!(defines_recipe("flash board='t114': fast", "flash"));

    // `just <recipe>` is what names a recipe; the bare token is not a
    // citation subject anywhere in the corpus.
    let naming = Naming::new(Corpus::Book);
    assert_eq!(
        attached_ident("the pin failed `just standard` (", &naming).as_deref(),
        Some("standard")
    );
    assert_eq!(
        attached_ident("`just flash-rak4631-pocket`, ", &naming).as_deref(),
        Some("flash-rak4631-pocket")
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let mention = 11;
    let cited = 12;
    let recipe = 53;
    let mut justfile = String::new();
    for _ in 1..mention {
        justfile.push_str("# padding\n");
    }
    justfile.push_str("# the gate has failed `just standard` on every sha since.\n");
    justfile.push_str("# cost 18 commits a landing gate and two red cycles.\n");
    for _ in (cited + 1)..recipe {
        justfile.push_str("# padding\n");
    }
    justfile.push_str("standard: fast test-ffi verify-packaging\n");
    justfile.push_str("    cargo test --workspace\n");
    fs::write(root.join("Justfile"), &justfile).unwrap();

    let docs = root.join("docs/src");
    fs::create_dir_all(&docs).unwrap();
    let doc = docs.join("gates.md");
    let cite = |line: usize| format!("The pin failed `just standard` (`Justfile:{line}`).\n");

    fs::write(&doc, cite(cited)).unwrap();
    let citations = scan(root, std::slice::from_ref(&doc), Corpus::Book);
    assert_eq!(citations.len(), 1, "the fixture citation was not parsed");
    assert_eq!(
        citations[0].ident.as_deref(),
        Some("standard"),
        "`just standard` did not name the citation beside it"
    );
    let (_, failures) = check(root, &citations);
    assert_eq!(
        failures.len(),
        1,
        "a citation onto a comment that merely mentions the recipe was accepted"
    );
    assert_eq!(failures[0].kind, FailureKind::Drift);
    assert!(
        failures[0].message.contains("recipe `standard`")
            && failures[0].message.contains(&format!("line {recipe}")),
        "the report does not point at the recipe's definition: {}",
        failures[0].message
    );

    // The other direction, so a check that stops firing is visible: the
    // citation that does land on the definition must stay quiet.
    fs::write(&doc, cite(recipe)).unwrap();
    let citations = scan(root, &[doc], Corpus::Book);
    let (_, failures) = check(root, &citations);
    let messages: Vec<&str> = failures.iter().map(|f| f.message.as_str()).collect();
    assert!(
        failures.is_empty(),
        "the citation onto the recipe header was reported: {}",
        messages.join("\n\n")
    );
}

/// Both spellings that attach a name to a citation are seen, and nothing
/// else became a name with them.
///
/// The false-positive side is the load-bearing half. A token wrongly read
/// as the citation's subject does not usually produce a red — it produces a
/// citation counted as drift-checked whose needle (`3`, `0001`) matches
/// something within the window by accident. That is worse than leaving it
/// bare, because the coverage number then says the citation is checked.
#[test]
fn a_citation_names_its_subject_in_either_spelling_and_in_nothing_else() {
    let naming = Naming::new(Corpus::Book);
    let ident = |before: &str| attached_ident(before, &naming);

    // The two spellings, including across a line break.
    assert_eq!(
        ident("derived by `resolve_lt_alock` ("),
        Some("resolve_lt_alock".into())
    );
    assert_eq!(
        ident("derived by (`resolve_lt_alock`, "),
        Some("resolve_lt_alock".into())
    );
    assert_eq!(ident("(`erp_band_gap`,\n"), Some("erp_band_gap".into()));
    // The function spelling loses its parens; `Type::method` and
    // `module.attr` keep theirs, because `check` searches the last segment.
    assert_eq!(ident("`airtime_ms()` ("), Some("airtime_ms".into()));
    assert_eq!(
        ident("`RadioConfig::eu_medium` ("),
        Some("RadioConfig::eu_medium".into())
    );

    // The table spelling: the name is inside a signature, several words
    // from the citation, and only a table row admits that distance.
    assert_eq!(
        ident("| `fn has_path(&self, dest_hash: &DestinationHash) -> bool` — "),
        Some("has_path".into())
    );
    assert_eq!(
        ident("| `Clock` | `fn now_ms(&self) -> u64` | "),
        Some("now_ms".into())
    );
    assert_eq!(
        ident("| `fn load<P: AsRef<Path>>(path: P) -> Result<Self>` — "),
        Some("load".into())
    );
    assert_eq!(
        ident("  /// | `async fn build(self) -> Result<N>` — "),
        Some("build".into())
    );

    for not_attached in [
        // Prose between the name and the citation: the pairing has to be
        // unambiguous, so an identifier mentioned earlier in the sentence
        // is not the citation's subject.
        "`Transport.outbound()` at ",
        "`Transport.outbound()` is the loop, and (",
        // A citation next to a citation, the shape a comparison table has.
        "| Self-announce one-shot | `Destination.py:322`, ",
        "the recipe moved (`Justfile:1167`, ",
        // A token with no letter in its last segment cannot be searched
        // for as an identifier.
        "the VID:PID `1209:0001` (",
        "`4.2` (",
        // Nothing at all: the bare citation, which stays existence-checked.
        "while the tracker is locked (",
        // A signature outside a table row: only a row puts the name and
        // the citation of that name several words apart on purpose, and
        // reading prose that far back is how a sentence's first
        // identifier gets read as a later citation's subject.
        "the accessor `fn has_path(&self) -> bool` is defined at ",
        // A row that mentions functions without declaring one.
        "| the `fn` above | ",
    ] {
        assert_eq!(
            ident(not_attached),
            None,
            "`{not_attached}` was read as naming the citation that follows it"
        );
    }

    // A row's second citation is bare: the signature is the subject of the
    // first one. Built rather than spelled out, because a literal citation
    // in this file would be scanned as part of the corpus it guards.
    let first = format!(
        "| `fn has_path(&self) -> bool` — `driver/mod.rs:{}` — ",
        2862
    );
    assert_eq!(ident(&first), None);
}

/// Guarantee C, kind 2: a figure a doc comment attributes to a document
/// must occur in that document (Codeberg #200).
#[test]
fn doc_comment_figures_are_on_the_page_they_cite() {
    run_figure_canary();

    let root = repo_root();
    let (triggered, checked, failures) = figure_attributions(&root, SOURCE_CRATES);

    // Published like the citation counts above, and for the same reason: a
    // trigger this narrow finds very little, and the number it found is the
    // only thing that tells a reader whether "no failures" means the tree is
    // clean or the trigger stopped firing. The canary is the real guard
    // against the second; this is what makes it visible without one.
    println!(
        "figure attributions ({}): {triggered} paragraph(s) naming a docs/ page \
         and quoting a decimal, {checked} figure(s) checked",
        SOURCE_CRATES.join(", ")
    );

    report("figure-attribution", &failures);
}

#[test]
fn doc_citations_resolve() {
    run_canary();

    let root = repo_root();
    let citations = book_citations(&root);
    let (counts, failures) = check(&root, &citations);

    println!(
        "doc citations: {} total, {} identifier-checked, {} bare \
         (existence/length only), {} external (unchecked)",
        counts.total(),
        counts.with_ident,
        counts.bare,
        counts.external
    );
    report_offsets("doc", &counts);

    // Tripwire against parser rot, not a coverage target: the corpus has
    // ~800 citations (~70 with identifiers) as of 2026-08. A guard that
    // silently stops matching is worse than none; if the docs shrink
    // deliberately, lower these floors in the same commit.
    assert!(
        counts.total() >= 300,
        "only {} citations parsed -- parser rot?",
        counts.total()
    );
    assert!(
        counts.with_ident >= 30,
        "only {} identifier citations parsed -- parser rot?",
        counts.with_ident
    );

    report("doc", &failures);
}

#[test]
fn source_citations_resolve() {
    run_canary();

    let root = repo_root();
    let citations = source_citations(&root, SOURCE_CRATES);
    let (counts, failures) = check(&root, &citations);

    // Published on every run, like the book's, so nobody reads a green guard
    // as full coverage. The bare majority is real: for those citations this
    // is existence-and-length checking, which catches renames and deletions
    // and not drift inside a file that stays long enough. Converting them to
    // the ``ident` (`path:line`)` form is editorial work (#167).
    println!(
        "source citations ({}): {} total, {} identifier-checked, {} bare \
         (existence/length only), {} external (unchecked)",
        SOURCE_CRATES.join(", "),
        counts.total(),
        counts.with_ident,
        counts.bare,
        counts.external
    );
    report_offsets("source", &counts);

    // Same tripwire role as the book floors above.
    assert!(
        counts.total() >= 500,
        "only {} source citations parsed -- parser rot?",
        counts.total()
    );
    assert!(
        counts.with_ident >= 30,
        "only {} identifier source citations parsed -- parser rot?",
        counts.with_ident
    );

    report("source", &failures);
}

#[test]
fn script_citations_resolve() {
    let root = repo_root();
    let citations = script_citations(&root);
    let (counts, failures) = check(&root, &citations);

    println!(
        "script citations (scripts/*.sh, Justfile): {} total, {} identifier-checked, \
         {} bare (existence/length only), {} external (unchecked)",
        counts.total(),
        counts.with_ident,
        counts.bare,
        counts.external
    );
    report_offsets("script", &counts);

    // Same tripwire role as the floors on the other two corpora, one order of
    // magnitude down: this corpus is small by nature (5 citations at
    // introduction), so the floor only has to catch "the walk stopped
    // returning files".
    assert!(
        counts.total() >= 3,
        "only {} script citations parsed -- parser rot?",
        counts.total()
    );

    report("script", &failures);
}

#[test]
fn concept_docs_reachable_from_summary() {
    let root = repo_root();
    let summary_path = root.join("docs/src/SUMMARY.md");
    let summary = fs::read_to_string(&summary_path).unwrap();
    let link_re = Regex::new(r"\]\(([^)]+\.md)\)").unwrap();

    let linked: BTreeSet<String> = link_re
        .captures_iter(&summary)
        .map(|c| c[1].to_string())
        .collect();

    // Every SUMMARY entry must point at an existing file: a dangling
    // entry renders as an empty chapter.
    let dangling: Vec<&String> = linked
        .iter()
        .filter(|l| !root.join("docs/src").join(l.as_str()).is_file())
        .collect();
    assert!(
        dangling.is_empty(),
        "SUMMARY.md links without a file behind them: {dangling:?}"
    );

    // Every concept document must be in SUMMARY.md: an unlisted file is
    // invisible in the built book, and an invisible policy document is
    // policy nobody reads.
    let on_disk: BTreeSet<String> = fs::read_dir(root.join("docs/src/concepts"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .map(|e| format!("concepts/{}", e.file_name().to_string_lossy()))
        .collect();
    let listed: BTreeSet<String> = linked
        .iter()
        .filter(|l| l.starts_with("concepts/"))
        .cloned()
        .collect();

    let orphaned: Vec<&String> = on_disk.difference(&listed).collect();
    assert!(
        orphaned.is_empty(),
        "concept document(s) not listed in docs/src/SUMMARY.md \
         (invisible in the built book): {orphaned:?}"
    );
}

// --- the bare half: anchoring a line number to what was on the line ------
//
// Everything above checks a citation that *names* what it points at. That is
// 764 of the corpus's 3228. The other 2464 name nothing, and for those the
// checks above are existence and length: the file is there and has at least
// that many lines. A citation that drifted from line 810 to 883 satisfies
// both forever, which is the whole defect — 59 % of the corpus guarded by a
// check that cannot see the thing that goes wrong with it.
//
// The name is not the only thing that survives a move. The *text of the
// cited line* does too, and unlike a name it is already there: no citation
// has to be rewritten to acquire one. So:
//
//   1. `git blame` the line the citation sits on -> commit C, the last
//      commit that wrote it. That is the newest moment anyone can be
//      assumed to have looked at the citation.
//   2. The cited file as of C, at the cited line -> the anchor, the text the
//      author was pointing at. For a citation into `reference/<submodule>`
//      that means the submodule's own history, read at the gitlink this tree
//      pinned in C.
//   3. The cited file now, at the same line. Same text (leading and trailing
//      whitespace ignored) -> the citation still points at what it pointed
//      at.
//   4. Otherwise, search the current file for the anchor text. Found at
//      exactly one other line -> the text is still there, at a line the
//      citation does not name. The number is wrong, and by how much.
//
// Only step 4's *unique* match is a failure, and that is deliberate: the
// anchor text is demonstrably in the file at a line the citation does not
// name, so the report needs no judgement about what the citing sentence
// meant. Everything else is left undecided and counted, in the open:
//
//   - the anchor matches several lines (`}`, `*/`): decides nothing;
//   - the anchor has vanished: the cited code was rewritten in place, which
//     may or may not have invalidated the sentence, and only a reader can
//     say. Reported as a count, not a failure — making it red would train
//     the reflex of re-pointing citations to make a gate green;
//   - the citing line is uncommitted, or the cited file did not exist at C.
//
// # What this cannot see, stated rather than hidden
//
// The baseline is the citing line's own last edit, so a citation that was
// already wrong when it was last written passes. Reflowing a paragraph
// re-baselines every citation in it. And a citation whose *sentence* went
// wrong while the cited line stayed put is invisible here, as it is to every
// other check in this file: Guarantee C is about references, not truth.
//
// So the number this reports is a floor. On the tree it was introduced
// against it was 387 — against the 126 an aborted merge sweep had touched,
// and against the 0 the guard had been reporting since it was written.

/// Run git in `repo`, returning stdout on success.
fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Line number -> the commit that last wrote that line.
///
/// `--line-porcelain` puts a full header in front of every line, so the
/// mapping is read off directly. A header is `<40 hex> <orig> <final>`; every
/// other line either starts with a tab (the content) or with a keyword.
///
/// The working tree, not `HEAD`: a citation edited but not yet committed is
/// blamed to the all-zero sha, and an unanchored citation is the honest
/// verdict for one nobody has committed yet. Blaming `HEAD` instead would
/// check the line the author just wrote against the anchor of the line it
/// replaced, and report every repair as fresh drift.
fn blame_commits(root: &Path, file: &Path) -> BTreeMap<usize, String> {
    let mut map = BTreeMap::new();
    let Some(out) = git(
        root,
        &["blame", "--line-porcelain", "--", &file.to_string_lossy()],
    ) else {
        return map;
    };
    for line in out.lines() {
        let mut fields = line.split(' ');
        let (Some(sha), Some(_), Some(final_line)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if let Ok(n) = final_line.parse::<usize>() {
            map.insert(n, sha.to_string());
        }
    }
    map
}

/// `<rev>:<path>` for many pairs at once.
///
/// One `git cat-file --batch` per repository rather than one `git show` per
/// pair: the corpus asks for roughly 1500 blobs and the process spawns, not
/// the reads, are what a gate would feel. Requests go in through a file so
/// there is no pipe to deadlock on.
fn read_blobs(
    repo: &Path,
    reqs: &BTreeSet<(String, String)>,
) -> BTreeMap<(String, String), Vec<String>> {
    let mut out = BTreeMap::new();
    if reqs.is_empty() {
        return out;
    }
    let Ok(mut req_file) = tempfile::NamedTempFile::new() else {
        return out;
    };
    {
        use std::io::Write;
        for (rev, path) in reqs {
            if writeln!(req_file, "{rev}:{path}").is_err() {
                return out;
            }
        }
        if req_file.flush().is_err() {
            return out;
        }
    }
    let Ok(stdin) = fs::File::open(req_file.path()) else {
        return out;
    };
    let Ok(finished) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(std::process::Stdio::from(stdin))
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return out;
    };
    let buf = finished.stdout;
    let mut pos = 0usize;
    for req in reqs {
        let Some(rel_nl) = buf[pos..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&buf[pos..pos + rel_nl]).into_owned();
        pos += rel_nl + 1;
        // `<oid> blob <size>` for a hit; `<request> missing` for anything
        // else -- a path not in that tree, or a rev that is not there.
        let Some(size) = header
            .contains(" blob ")
            .then(|| header.rsplit(' ').next())
            .flatten()
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        if pos + size > buf.len() {
            break;
        }
        let body = String::from_utf8_lossy(&buf[pos..pos + size]).into_owned();
        pos += size + 1;
        out.insert(req.clone(), body.lines().map(str::to_string).collect());
    }
    out
}

/// The reference commit this tree pinned for `sub` at `commit`.
///
/// The submodules lived under `vendor/` until 2026-07-12 (`7f52d1e6`), so a
/// citation whose line has not been touched since then is pinned at the old
/// path. Without the fallback 651 citations into the Python references are
/// undecidable rather than checked, which is most of what this exists for.
fn gitlink_at(root: &Path, commit: &str, sub: &str) -> Option<String> {
    ["reference", "vendor"].iter().find_map(|prefix| {
        git(root, &["rev-parse", &format!("{commit}:{prefix}/{sub}")])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// `("LXMF", "LXMF/LXMRouter.py")` for a path inside a reference submodule.
fn submodule_of(path: &str) -> Option<(&'static str, String)> {
    let rest = path.strip_prefix("reference/")?;
    SUBMODULES.iter().find_map(|sub| {
        rest.strip_prefix(sub)
            .and_then(|r| r.strip_prefix('/'))
            .map(|inner| (*sub, inner.to_string()))
    })
}

/// What became of one cited line.
#[derive(Clone, PartialEq, Eq)]
enum Anchor {
    /// The line still holds the text it held when the citation was written.
    Fresh,
    /// That text is now, uniquely, at this line. The citation is wrong.
    Moved(usize),
    /// That text is gone from the file. A reader has to decide.
    Rewritten,
    /// That text is now on several lines (`else:`, `}`, `]`), so on its own
    /// it places nothing. [`place_by_common_shift`] can still place it if the
    /// rest of the citation agrees on where the block went.
    Several(Vec<usize>),
    /// Nothing to compare: a blank line, no history for the citing line, or
    /// the cited file absent at the citing commit.
    Undecided(&'static str),
}

/// Where the cited line sits in `new` when it is read together with the line
/// above and the line below it.
///
/// A line like `else:`, `}` or `]` is the same as forty other lines in the
/// file and places nothing on its own. With its two neighbours it usually
/// places exactly one spot, and that is still the citation's own text rather
/// than a guess about it. Only a unique match counts; two matches are as
/// undecidable as before.
fn unique_context_line(old: &[String], new: &[String], line: usize) -> Option<usize> {
    let window = |v: &[String], centre: usize| -> Option<Vec<String>> {
        let lo = centre.checked_sub(2)?;
        let hi = (centre + 1).min(v.len());
        (hi > centre).then(|| v[lo..hi].iter().map(|l| l.trim().to_string()).collect())
    };
    let needle = window(old, line)?;
    if needle.len() < 2 {
        return None;
    }
    let mut found = None;
    for centre in 2..=new.len() {
        if window(new, centre).as_ref() == Some(&needle) {
            if found.is_some() {
                return None;
            }
            found = Some(centre);
        }
    }
    found
}

fn anchor_verdict(old: &[String], new: &[String], line: usize) -> Anchor {
    if old.len() < line {
        return Anchor::Undecided("the cited file was shorter than the citation when cited");
    }
    if new.len() < line {
        return Anchor::Undecided("the cited file is shorter than the citation now");
    }
    let anchor = old[line - 1].trim();
    if anchor.is_empty() {
        return Anchor::Undecided("the cited line was blank when it was cited");
    }
    if new[line - 1].trim() == anchor {
        return Anchor::Fresh;
    }
    let hits: Vec<usize> = new
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == anchor)
        .map(|(i, _)| i + 1)
        .collect();
    match hits.len() {
        0 => Anchor::Rewritten,
        1 => Anchor::Moved(hits[0]),
        _ => match unique_context_line(old, new, line) {
            Some(only) => Anchor::Moved(only),
            None => Anchor::Several(hits),
        },
    }
}

/// Place the endpoints a single anchor could not place, when the rest of the
/// same citation agrees on one displacement.
///
/// A citation like `Identity.py:768-777` can have its first line move
/// unambiguously while its last line is blank, or is a `}` that occurs forty
/// times. That endpoint is unplaceable on its own -- but if every endpoint
/// the anchor DID place moved by the same number of lines, the line that
/// number away can simply be looked at: if it holds this endpoint's text, the
/// endpoint is placed by evidence, not by inference. A blank endpoint lands
/// on a blank line the same way.
///
/// Nothing is assumed. An endpoint whose text is not at the displaced line
/// stays unplaced, and a citation with two different displacements in it gets
/// no help at all -- that is a citation somebody has to read.
fn place_by_common_shift(spans: &mut [AnchoredSpan], new: &[String]) {
    let shifts: BTreeSet<isize> = spans
        .iter()
        .filter(|s| !s.weak)
        .filter_map(|s| match s.verdict {
            Anchor::Moved(to) => Some(to as isize - s.line as isize),
            _ => None,
        })
        .collect();
    let [shift] = shifts.into_iter().collect::<Vec<_>>()[..] else {
        return;
    };
    for s in spans.iter_mut() {
        if s.verdict == Anchor::Fresh || (!s.weak && matches!(s.verdict, Anchor::Moved(_))) {
            continue;
        }
        let target = s.line as isize + shift;
        // A wordless endpoint the neighbour-context rule already placed is
        // corroborated if it landed where the shift says. Without this it
        // would be discarded below and a range ending on a `}` would stay
        // unrepairable however plainly its block had moved -- which is what
        // the injected-drift control found.
        if let Anchor::Moved(to) = s.verdict {
            if to as isize == target {
                s.weak = false;
            }
            continue;
        }
        if target < 1 || target as usize > new.len() {
            continue;
        }
        match &s.verdict {
            // The displaced line holds this endpoint's text: placed. A
            // wordless anchor that lands here is corroborated and stops
            // being weak.
            _ if new[target as usize - 1].trim() == s.anchor => {
                s.verdict = Anchor::Moved(target as usize);
                s.weak = false;
            }
            // The text is on several lines, and the block it ends changed
            // length, so the displaced line is not one of them. Exactly one
            // occurrence near where the rest of the citation went is still
            // evidence: `Transport.py:1722-1764` ends on a `]` that the
            // reference now has four of, and only one of them is a line away
            // from the +145 its opening line moved. Two candidates that close
            // would not be evidence, and neither would one far away, so both
            // place nothing. The tolerance is `WINDOW`, the same slack the
            // identifier check allows between a citation and its subject.
            Anchor::Several(hits) => {
                let near: Vec<usize> = hits
                    .iter()
                    .copied()
                    .filter(|&h| (h as isize - target).unsigned_abs() <= WINDOW)
                    .collect();
                if let [only] = near[..] {
                    s.verdict = Anchor::Moved(only);
                    s.weak = false;
                }
            }
            _ => {}
        }
    }
}

/// One endpoint of one span of one citation, with what became of it.
struct AnchoredSpan {
    span: usize,
    /// 0 for the start of the span, 1 for its end.
    edge: usize,
    line: usize,
    anchor: String,
    verdict: Anchor,
    /// The anchor carries no word: `"""`, `}`, `]`, `);`. Nobody cites a
    /// delimiter on purpose, so finding one somewhere else is no evidence
    /// that the citation drifted -- `Identity.py:84,383` pointed at a stray
    /// `"""` the day it was written and at the right constant today, and
    /// "repairing" it to where that `"""` went would have broken a correct
    /// citation. A wordless anchor therefore never establishes drift by
    /// itself; [`place_by_common_shift`] may still carry it along once the
    /// rest of the citation has established where the block went, which is
    /// what keeps a range ending on a `]` repairable.
    weak: bool,
}

/// Whether the anchor text can carry a citation at all.
fn wordless(anchor: &str) -> bool {
    !anchor.chars().any(|c| c.is_alphanumeric())
}

#[derive(Default)]
struct AnchorCounts {
    fresh: usize,
    moved: usize,
    rewritten: usize,
    undecided: usize,
    external: usize,
    unresolved: usize,
}

/// Anchor every bare citation in `citations` against the tree's own history.
///
/// Returns the per-endpoint verdicts for the citations that have at least one
/// moved endpoint, plus the counts over all of them.
fn anchor_bare_citations<'a>(
    root: &Path,
    citations: &'a [Citation],
) -> (AnchorCounts, Vec<(&'a Citation, Vec<AnchoredSpan>)>) {
    let mut counts = AnchorCounts::default();
    let mut drifted = Vec::new();

    let mut index = Vec::new();
    walk(root, SKIP_DIRS, &mut index);
    let rel_index: Vec<String> = index
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    // One blame per citing file, not per citation: 233 files carry the 2464
    // bare citations between them.
    let mut blame: BTreeMap<PathBuf, BTreeMap<usize, String>> = BTreeMap::new();
    // (candidate path, citing commit) for every bare citation, resolved.
    struct Pending<'a> {
        citation: &'a Citation,
        /// Every file the cited name resolves to. A bare `constants.rs:9`
        /// matches several, and the prose rather than the path disambiguates
        /// -- so, exactly as `check` does it, the citation passes if ANY
        /// candidate still holds its text, and drift is only reported when
        /// no candidate does.
        cands: Vec<String>,
        commit: String,
    }
    let mut pending = Vec::new();

    for c in citations {
        if c.ident.is_some() {
            continue;
        }
        if EXTERNAL_PREFIXES.iter().any(|p| c.path.starts_with(p)) {
            counts.external += 1;
            continue;
        }
        let suffix = format!("/{}", c.path);
        let cands: Vec<String> = rel_index
            .iter()
            .filter(|f| **f == c.path || f.ends_with(&suffix))
            .cloned()
            .collect();
        if cands.is_empty() {
            // Already a hard failure in `check`; not this check's business.
            counts.unresolved += 1;
            continue;
        }
        let commits = blame
            .entry(c.doc.clone())
            .or_insert_with(|| blame_commits(root, &c.doc));
        let Some(commit) = commits.get(&c.doc_line) else {
            counts.undecided += c.spans.len();
            continue;
        };
        if commit.chars().all(|ch| ch == '0') {
            // The citing line is not committed yet: nothing to anchor to.
            counts.undecided += c.spans.len();
            continue;
        }
        pending.push(Pending {
            citation: c,
            cands,
            commit: commit.clone(),
        });
    }

    // Gitlinks first: a citation into a reference submodule is read out of
    // that submodule's history, at the commit this tree pinned back then.
    let mut gitlinks: BTreeMap<(String, &str), Option<String>> = BTreeMap::new();
    for p in &pending {
        for cand in &p.cands {
            if let Some((sub, _)) = submodule_of(cand) {
                let key = (p.commit.clone(), sub);
                gitlinks
                    .entry(key)
                    .or_insert_with(|| gitlink_at(root, &p.commit, sub));
            }
        }
    }

    // Batch the blob reads, grouped by the repository they come from.
    let mut want: BTreeMap<PathBuf, BTreeSet<(String, String)>> = BTreeMap::new();
    let repo_and_path = |commit: &str, cand: &str| -> Option<(PathBuf, String, String)> {
        match submodule_of(cand) {
            Some((sub, inner)) => {
                let link = gitlinks.get(&(commit.to_string(), sub))?.clone()?;
                Some((root.join("reference").join(sub), link, inner))
            }
            None => Some((root.to_path_buf(), commit.to_string(), cand.to_string())),
        }
    };
    for p in &pending {
        for cand in &p.cands {
            if let Some((repo, rev, path)) = repo_and_path(&p.commit, cand) {
                want.entry(repo).or_default().insert((rev, path));
            }
        }
    }
    let blobs: BTreeMap<PathBuf, BTreeMap<(String, String), Vec<String>>> = want
        .iter()
        .map(|(repo, reqs)| (repo.clone(), read_blobs(repo, reqs)))
        .collect();

    for p in &pending {
        let mut per_candidate = Vec::new();
        for cand in &p.cands {
            let Some((repo, rev, path)) = repo_and_path(&p.commit, cand) else {
                continue;
            };
            // "Then" comes out of history; "now" comes off disk, because the
            // working tree is what a reader following the citation opens.
            let Some(old) = blobs[&repo].get(&(rev, path)) else {
                continue;
            };
            let Ok(text) = fs::read_to_string(root.join(cand)) else {
                continue;
            };
            let new: Vec<String> = text.lines().map(str::to_string).collect();
            let new = &new;
            let mut spans: Vec<AnchoredSpan> = Vec::new();
            for (i, &(start, end)) in p.citation.spans.iter().enumerate() {
                for (edge, line) in [(0, start), (1, end)] {
                    if edge == 1 && end == start {
                        continue;
                    }
                    spans.push(AnchoredSpan {
                        span: i,
                        edge,
                        line,
                        anchor: old
                            .get(line - 1)
                            .map(|l| l.trim().to_string())
                            .unwrap_or_default(),
                        verdict: anchor_verdict(old, new, line),
                        weak: old.get(line - 1).is_some_and(|l| wordless(l.trim())),
                    });
                }
            }
            place_by_common_shift(&mut spans, new);
            // Whatever the shift did not corroborate, a wordless anchor
            // cannot claim on its own.
            for s in spans.iter_mut() {
                if s.weak && matches!(s.verdict, Anchor::Moved(_)) {
                    s.verdict = Anchor::Undecided("the cited line carries no word to anchor to");
                }
            }
            per_candidate.push(spans);
        }

        // A candidate that still holds the citation's text, with nothing
        // moved, settles it: the citation is right about that file, whatever
        // the same-named file one directory over now says.
        let settled = per_candidate.iter().position(|spans| {
            !spans.iter().any(|s| matches!(s.verdict, Anchor::Moved(_)))
                && spans.iter().any(|s| s.verdict == Anchor::Fresh)
        });
        let chosen = settled.or_else(|| {
            per_candidate
                .iter()
                .position(|spans| spans.iter().any(|s| matches!(s.verdict, Anchor::Moved(_))))
        });
        let Some(chosen) = chosen else {
            counts.undecided += per_candidate
                .first()
                .map_or(p.citation.spans.len(), |s| s.len());
            continue;
        };
        let spans = &per_candidate[chosen];
        for s in spans {
            match s.verdict {
                Anchor::Fresh => counts.fresh += 1,
                Anchor::Moved(_) => counts.moved += 1,
                Anchor::Rewritten => counts.rewritten += 1,
                Anchor::Several(_) | Anchor::Undecided(_) => counts.undecided += 1,
            }
        }
        if settled.is_none() {
            drifted.push((p.citation, per_candidate.swap_remove(chosen)));
        }
    }

    (counts, drifted)
}

/// The citation rewritten with every endpoint moved to where its text went.
///
/// `None` when some endpoint of the citation could not be decided: half a
/// repaired span would be worse than the drift it replaces.
fn repaired(c: &Citation, spans: &[AnchoredSpan]) -> Option<String> {
    let mut fixed = c.spans.clone();
    for s in spans {
        match s.verdict {
            Anchor::Moved(to) => {
                // A single-line span carries only its start edge, so moving
                // that edge has to move both ends or the span inverts.
                let point = c.spans.get(s.span).is_some_and(|&(a, b)| a == b);
                let slot = fixed.get_mut(s.span)?;
                if s.edge == 0 {
                    slot.0 = to;
                    if point {
                        slot.1 = to;
                    }
                } else {
                    slot.1 = to;
                }
            }
            Anchor::Fresh => {}
            _ => return None,
        }
    }
    if fixed.iter().any(|&(a, b)| a > b) {
        return None;
    }
    let spec = fixed
        .iter()
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let quote = if c.raw.starts_with('`') { "`" } else { "" };
    Some(format!("{quote}{}:{spec}{quote}", c.path))
}

/// Rewrite every repairable citation in place. Returns how many it rewrote.
///
/// Off unless `LEVICULUM_CITATION_FIX` is set. The finder and the fixer are
/// the same code on purpose: a separate fixing script would be a second
/// implementation of the anchor rule, and the first symptom of the two
/// disagreeing is a repair pointed at the wrong line.
fn apply_repairs(root: &Path, drifted: &[(&Citation, Vec<AnchoredSpan>)]) -> usize {
    let mut per_file: BTreeMap<PathBuf, Vec<(usize, usize, String)>> = BTreeMap::new();
    for (c, spans) in drifted {
        if let Some(new) = repaired(c, spans) {
            per_file
                .entry(c.doc.clone())
                .or_default()
                .push((c.offset, c.raw.len(), new));
        }
    }
    let mut n = 0;
    for (doc, mut edits) in per_file {
        let path = root.join(&doc);
        let Ok(mut text) = fs::read_to_string(&path) else {
            continue;
        };
        // Back to front, so an earlier edit does not move a later offset.
        edits.sort_by_key(|e| std::cmp::Reverse(e.0));
        for (offset, len, new) in edits {
            if text.get(offset..offset + len).is_none() {
                continue;
            }
            text.replace_range(offset..offset + len, &new);
            n += 1;
        }
        let _ = fs::write(&path, text);
    }
    n
}

fn anchor_report(
    label: &str,
    counts: &AnchorCounts,
    drifted: &[(&Citation, Vec<AnchoredSpan>)],
) -> Vec<Failure> {
    println!(
        "{label} bare-citation anchors: {} cited lines still hold the text they held \
         when cited, {} moved, {} rewritten in place, {} undecidable; {} external, \
         {} unresolved",
        counts.fresh,
        counts.moved,
        counts.rewritten,
        counts.undecided,
        counts.external,
        counts.unresolved
    );
    drifted
        .iter()
        .map(|(c, spans)| {
            let mut lines = vec![format!("{}:{}: {}", c.doc.display(), c.doc_line, c.raw)];
            for s in spans.iter() {
                if let Anchor::Moved(to) = s.verdict {
                    lines.push(format!(
                        "    line {} held `{}` when this citation was last written; \
                         that text is now at line {} ({:+})",
                        s.line,
                        s.anchor.chars().take(90).collect::<String>(),
                        to,
                        to as isize - s.line as isize
                    ));
                }
            }
            match repaired(c, spans) {
                Some(new) => lines.push(format!("    the citation should read {new}")),
                None => lines.push(
                    "    not all of this citation's endpoints could be placed; \
                     re-read it rather than renumbering it"
                        .into(),
                ),
            }
            Failure {
                kind: FailureKind::Drift,
                message: lines.join("\n"),
            }
        })
        .collect()
}

/// The standing canary for the anchor rule: a miniature repo with two bare
/// citations, one of which is made wrong by a commit that moves the code
/// under it.
///
/// The concept page requires a permanent pair, not a one-time demonstration.
/// This check is made of subprocess calls into git and a text comparison, and
/// every one of those failure modes -- blame that returns nothing, a
/// `cat-file` batch that desynchronises, a resolution that finds no candidate
/// -- produces *no findings*, which reads exactly like a clean tree. So both
/// directions are asserted on the same fixture: the moved citation must be
/// reported with the line its text moved to, and the citation whose line did
/// not move must not be reported at all.
fn run_bare_anchor_canary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let git_in = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=canary",
                "-c",
                "user.email=canary@localhost",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "CANARY: git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    let src = root.join("leviculum-core/src");
    fs::create_dir_all(&src).unwrap();
    let docs = root.join("docs/src/concepts");
    fs::create_dir_all(&docs).unwrap();

    // Line 5 is the one that stays put; line 10 is the one that moves.
    let mut body: Vec<String> = (1..=20).map(|n| format!("// filler {n}")).collect();
    body[4] = "pub const CANARY_ANCHOR: u8 = 7;".into();
    body[9] = "pub fn canary_moves() {}".into();
    // The end of a range, and wordless: only the shift its opening line
    // establishes can place it.
    body[19] = "}".into();
    let code = src.join("canary_anchor.rs");
    fs::write(&code, body.join("\n") + "\n").unwrap();

    // Built rather than spelled out: a literal citation here would be scanned
    // as part of the corpus this file guards.
    let stays = format!("`canary_anchor.rs:{}`", 5);
    let moves = format!("`canary_anchor.rs:{}`", 10);
    let range = format!("`canary_anchor.rs:{}-{}`", 10, 20);
    let doc = docs.join("canary_anchor.md");
    fs::write(
        &doc,
        format!(
            "The constant ({stays}), the function ({moves}) and the block \
             ({range}) are all cited bare.\n"
        ),
    )
    .unwrap();

    git_in(&["init", "-q"]);
    git_in(&["add", "-A"]);
    git_in(&["commit", "-qm", "fixture"]);

    // Four lines land between the two cited lines. Nothing above line 5
    // changes, so that citation is still right; the function slides to 14.
    body.splice(6..6, (1..=4).map(|n| format!("// inserted {n}")));
    fs::write(&code, body.join("\n") + "\n").unwrap();
    git_in(&["add", "-A"]);
    git_in(&["commit", "-qm", "move the function down"]);

    let citations = scan(root, &[doc], Corpus::Book);
    assert_eq!(
        citations.len(),
        3,
        "CANARY: the fixture's three citations were not all parsed"
    );
    let (counts, drifted) = anchor_bare_citations(root, &citations);
    assert_eq!(
        counts.fresh, 1,
        "CANARY: {} of the 1 unmoved citation was seen as still pointing at its \
         text. Anchoring has stopped resolving, and a green run means nothing.",
        counts.fresh
    );
    assert_eq!(
        drifted.len(),
        2,
        "CANARY: {} citations reported, expected exactly the two moved ones. \
         This is the defect the check exists for.",
        drifted.len()
    );
    // The range, whose closing `}` no anchor of its own can place: only the
    // shift its opening line establishes carries it. Without that the fixer
    // gives up on every range ending in a brace, which an injected-drift
    // control found it doing.
    let (block, block_spans) = drifted
        .iter()
        .find(|(c, _)| c.raw.contains('-'))
        .expect("CANARY: the range citation was not reported");
    assert_eq!(
        repaired(block, block_spans).as_deref(),
        Some(format!("`canary_anchor.rs:{}-{}`", 14, 24).as_str()),
        "CANARY: a range whose end is a wordless line was not carried by the \
         displacement its start established"
    );
    let (cited, spans) = drifted
        .iter()
        .find(|(c, _)| !c.raw.contains('-'))
        .expect("CANARY: the single-line moved citation was not reported");
    assert!(
        cited.raw.contains(&moves[1..moves.len() - 1]),
        "CANARY: the wrong citation was reported: {}",
        cited.raw
    );
    assert_eq!(
        spans
            .iter()
            .filter_map(|s| match s.verdict {
                Anchor::Moved(to) => Some(to),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![14],
        "CANARY: the moved citation was reported without the line its text moved to"
    );
    assert_eq!(
        repaired(cited, spans).as_deref(),
        Some(format!("`canary_anchor.rs:{}`", 14).as_str()),
        "CANARY: the repair does not name the line the text actually moved to"
    );
}

/// A bare citation's line number still points at the text it pointed at.
///
/// The half of Guarantee C that was unguarded: see the block comment above
/// for the rule and for what it deliberately leaves undecided.
#[test]
fn bare_citations_still_point_at_the_text_they_cited() {
    run_bare_anchor_canary();

    let root = repo_root();
    if git(&root, &["rev-parse", "--git-dir"]).is_none() {
        // Loudly, not silently: a guard that quietly checks nothing is the
        // shape this whole check exists to remove.
        println!(
            "bare-citation anchors: SKIPPED -- {} is not a git checkout, so there is \
             no history to anchor a line number against.",
            root.display()
        );
        return;
    }
    if git(&root, &["rev-parse", "--is-shallow-repository"]).is_some_and(|s| s.trim() == "true") {
        println!(
            "bare-citation anchors: SKIPPED -- shallow clone. `git fetch --unshallow` \
             to check the 2464 bare citations this tree cannot otherwise see."
        );
        return;
    }

    let mut failures = Vec::new();
    let mut fixed = 0usize;
    for (label, citations) in [
        ("doc", book_citations(&root)),
        ("source", source_citations(&root, SOURCE_CRATES)),
    ] {
        let (counts, drifted) = anchor_bare_citations(&root, &citations);
        if std::env::var_os("LEVICULUM_CITATION_FIX").is_some() {
            fixed += apply_repairs(&root, &drifted);
        }
        failures.extend(anchor_report(label, &counts, &drifted));
    }
    if std::env::var_os("LEVICULUM_CITATION_FIX").is_some() {
        println!("LEVICULUM_CITATION_FIX: rewrote {fixed} citation(s)");
        return;
    }
    report("bare-citation anchor", &failures);
}
