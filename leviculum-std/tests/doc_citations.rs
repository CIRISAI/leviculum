//! Citation guard (Guarantee C): a reference to something outside the text
//! must still point at what it claims. Covers `docs/src/**/*.md` and the
//! Rust sources and tests of `leviculum-core`, `leviculum-lxmf`,
//! `leviculum-lxmf-node` and `leviculum-std`; and every concept document
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
//! - the cited file exists in the repo and has at least the cited number
//!   of lines (catches deletions and renames);
//! - where the citation follows a backticked identifier — the
//!   ``ident` (`path:line`)` convention — that identifier occurs within
//!   `WINDOW` lines of the cited span (catches drift).
//!
//! A bare citation with no adjacent identifier only gets the existence
//! check. Both kinds are counted and printed so the coverage is visible:
//! run with `--nocapture` to see the counts.
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
use std::collections::BTreeSet;
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
const EXTERNAL_PREFIXES: &[&str] = &["periculum/"];

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
    raw: String,
    path: String,
    /// Inclusive line spans: `329` → [(329,329)], `155-199,204` →
    /// [(155,199),(204,204)].
    spans: Vec<(usize, usize)>,
    /// The backticked identifier immediately preceding the citation, if
    /// the text uses the ``ident` (`path:line`)` convention.
    ident: Option<String>,
}

/// A path with a letters-only extension (or the extensionless `Justfile`)
/// followed by `:` and a line spec. The extension must be alphabetic so
/// `127.0.0.1:4242` and Python slices like `packed[:16]` do not match.
const PATH_PATTERN: &str = r"([A-Za-z0-9_][A-Za-z0-9_./-]*\.[A-Za-z]+|Justfile)";
const SPEC_PATTERN: &str = r"(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)";

fn cite_regex(corpus: Corpus) -> Regex {
    let body = format!("{PATH_PATTERN}:{SPEC_PATTERN}");
    match corpus {
        Corpus::Book => Regex::new(&format!("`{body}`")).unwrap(),
        // `\b` on the left stops a longer word ending in a cited filename from
        // matching at the inner offset; the right side is anchored by the line
        // spec.
        Corpus::Source => Regex::new(&format!(r"\b{body}")).unwrap(),
    }
}

/// ``ident` (` directly before the citation, whitespace (incl. line breaks)
/// allowed. A trailing `()` (function spelling) is stripped.
fn ident_regex() -> Regex {
    Regex::new(r"`([A-Za-z0-9_:.]+)(?:\(\))?`\s*\(\s*$").unwrap()
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
    let ident_re = ident_regex();
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
                raw: whole.as_str().to_string(),
                path: m[1].to_string(),
                spans,
                ident: ident_re
                    .captures(&text[..whole.start()])
                    .map(|c| c[1].to_string()),
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

/// Distance in lines from `line` to the nearest edge of `span` (0 when
/// inside).
fn span_distance(line: usize, span: (usize, usize)) -> usize {
    if line < span.0 {
        span.0 - line
    } else {
        line.saturating_sub(span.1)
    }
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
}

struct Failure {
    kind: FailureKind,
    message: String,
}

#[derive(Default)]
struct Counts {
    with_ident: usize,
    bare: usize,
    external: usize,
}

impl Counts {
    fn total(&self) -> usize {
        self.with_ident + self.bare + self.external
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
    // Bare filenames like `transport.rs:207` resolve by suffix.
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
            let hits: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.contains(needle))
                .map(|(i, _)| i + 1)
                .collect();
            let in_window = hits
                .iter()
                .any(|&h| c.spans.iter().any(|&span| span_distance(h, span) <= WINDOW));
            if in_window {
                passed = true;
                break;
            }
            let cited_first = c.spans[0].0;
            let nearest = hits
                .iter()
                .min_by_key(|&&h| c.spans.iter().map(|&s| span_distance(h, s)).min().unwrap());
            candidate_notes.push(match nearest {
                Some(&n) => format!(
                    "    `{needle}` not within {WINDOW} lines of the cited span in {cand}\n    cited line {cited_first}: {}\n    nearest `{needle}`: line {n}: {}",
                    lines[cited_first - 1].trim(),
                    lines[n - 1].trim()
                ),
                None => format!("    `{needle}` does not occur anywhere in {cand}"),
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
/// the four fixture citations is for.
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
        4,
        "CANARY: the parser found {} of 4 fixture citations. It has stopped \
         matching; every green run since it broke means nothing.",
        citations.len()
    );

    let (counts, failures) = check(root, &citations);
    assert_eq!(
        counts.with_ident, 4,
        "CANARY: identifier detection stopped working ({} of 4 seen), which \
         silently downgrades every citation to an existence check.",
        counts.with_ident
    );

    let kinds: Vec<&FailureKind> = failures.iter().map(|f| &f.kind).collect();
    assert_eq!(
        kinds.len(),
        3,
        "CANARY: expected exactly 3 failures (drift, missing, absent \
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
    // The drifted citation must be reported: this is the failure the guard
    // exists for, and the one that decays silently.
    assert!(
        failures
            .iter()
            .any(|f| f.kind == FailureKind::Drift && f.message.contains(&drifted)),
        "CANARY: a deliberately drifted citation was NOT reported. The guard \
         cannot see the defect it exists to catch."
    );
    // The correct one must not be, or the guard is noise and gets disabled.
    assert!(
        !failures
            .iter()
            .any(|f| f.message.contains(&format!("({correct})"))),
        "CANARY: a correct citation was reported as broken."
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
