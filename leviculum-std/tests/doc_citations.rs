//! Doc-citation guard: `path:line` citations in `docs/src/**/*.md` must
//! still point at what they claim, and every concept document must be
//! reachable from `docs/src/SUMMARY.md`.
//!
//! The concept documents are binding policy, and a wrong citation gets
//! believed. A 2026-07 audit found six drifted citations across five
//! documents after roughly one month; nothing else catches this, because
//! a drifted citation looks exactly like a fresh one.
//!
//! What is checked, per citation:
//! - the cited file exists in the repo and has at least the cited number
//!   of lines (catches deletions and renames);
//! - where the citation follows a backticked identifier — the
//!   ``ident` (`path:line`)` convention — that identifier occurs within
//!   `WINDOW` lines of the cited span (catches drift).
//!
//! A bare citation with no adjacent identifier only gets the existence
//! check. Both kinds are counted and printed so the coverage is visible:
//! run with `--nocapture` to see the counts.

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

/// One `path:line[-line][,line[-line]…]` citation found in a document.
struct Citation {
    doc: PathBuf,
    doc_line: usize,
    raw: String,
    path: String,
    /// Inclusive line spans: `329` → [(329,329)], `155-199,204` →
    /// [(155,199),(204,204)].
    spans: Vec<(usize, usize)>,
    /// The backticked identifier immediately preceding the citation, if
    /// the document uses the ``ident` (`path:line`)` convention.
    ident: Option<String>,
}

fn parse_citations(root: &Path) -> Vec<Citation> {
    // A path with a letters-only extension (or the extensionless
    // `Justfile`) followed by `:` and a line spec, all inside backticks.
    // The extension must be alphabetic so `127.0.0.1:4242` and Python
    // slices like `packed[:16]` do not match.
    let cite_re = Regex::new(
        r"`([A-Za-z0-9_][A-Za-z0-9_./-]*\.[A-Za-z]+|Justfile):(\d+(?:-\d+)?(?:,\d+(?:-\d+)?)*)`",
    )
    .unwrap();
    // ``ident` (` directly before the citation, whitespace (incl. line
    // breaks) allowed. A trailing `()` (function spelling) is stripped.
    let ident_re = Regex::new(r"`([A-Za-z0-9_:.]+)(?:\(\))?`\s*\(\s*$").unwrap();

    let mut mds = Vec::new();
    walk(&root.join("docs/src"), &[], &mut mds);
    mds.retain(|p| p.extension().is_some_and(|e| e == "md"));
    mds.sort();
    assert!(
        !mds.is_empty(),
        "no markdown files under docs/src -- wrong repo root?"
    );

    let mut citations = Vec::new();
    for md in &mds {
        let text = fs::read_to_string(md).unwrap();
        for m in cite_re.captures_iter(&text) {
            let whole = m.get(0).unwrap();
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
                doc: md.strip_prefix(root).unwrap().to_path_buf(),
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

/// Distance in lines from `line` to the nearest edge of `span` (0 when
/// inside).
fn span_distance(line: usize, span: (usize, usize)) -> usize {
    if line < span.0 {
        span.0 - line
    } else {
        line.saturating_sub(span.1)
    }
}

#[test]
fn doc_citations_resolve() {
    let root = repo_root();
    let citations = parse_citations(&root);

    // Index every repo file (submodules included) so bare filenames like
    // `transport.rs:207` resolve by suffix. VCS state, build output and
    // the rendered book are excluded; `docs/book` would otherwise shadow
    // the sources.
    let mut files = Vec::new();
    walk(
        &root,
        &[".git", "target", "book", "node_modules"],
        &mut files,
    );
    let rel_files: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let resolve = |cited: &str| -> Vec<&str> {
        let suffix = format!("/{cited}");
        rel_files
            .iter()
            .filter(|f| *f == cited || f.ends_with(&suffix))
            .map(String::as_str)
            .collect()
    };

    let submodules_missing = ["Reticulum", "LXMF", "LXST", "RNode_Firmware"]
        .into_iter()
        .any(|s| {
            fs::read_dir(root.join("reference").join(s))
                .map(|mut d| d.next().is_none())
                .unwrap_or(true)
        });

    let mut with_ident = 0usize;
    let mut bare = 0usize;
    let mut external = 0usize;
    let mut failures = Vec::new();

    for c in &citations {
        if EXTERNAL_PREFIXES.iter().any(|p| c.path.starts_with(p)) {
            external += 1;
            continue;
        }
        match c.ident {
            Some(_) => with_ident += 1,
            None => bare += 1,
        }
        let where_ = format!("{}:{}: {}", c.doc.display(), c.doc_line, c.raw);

        let candidates = resolve(&c.path);
        if candidates.is_empty() {
            let hint = if submodules_missing {
                "\n    (reference/ submodules are not checked out -- run \
                 `git submodule update --init` before trusting this failure)"
            } else {
                ""
            };
            failures.push(format!(
                "{where_}\n    no file matching `{}` in the repo -- deleted or renamed?{hint}",
                c.path
            ));
            continue;
        }

        // A citation passes if any candidate file satisfies every check;
        // bare filenames can be ambiguous (two `constants.rs` exist) and
        // the document's prose, not the path, disambiguates.
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
            failures.push(format!("{where_}\n{}", candidate_notes.join("\n")));
        }
    }

    let total = with_ident + bare + external;
    println!(
        "doc citations: {total} total, {with_ident} identifier-checked, \
         {bare} bare (existence/length only), {external} external (unchecked)"
    );

    // Tripwire against parser rot, not a coverage target: the corpus has
    // ~750 citations (~60 with identifiers) as of 2026-08. A guard that
    // silently stops matching is worse than none; if the docs shrink
    // deliberately, lower these floors in the same commit.
    assert!(total >= 300, "only {total} citations parsed -- parser rot?");
    assert!(
        with_ident >= 30,
        "only {with_ident} identifier citations parsed -- parser rot?"
    );

    assert!(
        failures.is_empty(),
        "{} doc citation(s) no longer point at what they claim.\n\
         Fix the document, not the guard: each entry names the document \
         line, the citation as written, and the nearest current match.\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
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
