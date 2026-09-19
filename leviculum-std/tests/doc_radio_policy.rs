//! Doc guard: the book may not describe a radio-regulatory refusal.
//!
//! Project policy, decided 2026-08-16 (Codeberg #257):
//!
//!   No radio configuration is ever refused for a radio-regulatory reason.
//!   It is warned about, loudly, and then honoured.
//!
//! The reason is not only that this stack cannot know the operator's
//! jurisdiction. In the EU the operator, not the manufacturer, carries the
//! legal responsibility for compliant operation, so software that refuses a
//! setting takes on a responsibility it does not hold — and blocks operation
//! that is lawful elsewhere, under a licence, or in a shielded chamber.
//!
//! The code half of that policy is already mechanical: the behavioural guard
//! in `leviculum-std/src/driver/interface_build/mod.rs`
//! (`no_radio_configuration_is_refused_for_a_regulatory_reason`) drives the
//! known regulatory edge cases through the config-building entry point and
//! asserts each one builds and warns. What it cannot see is the book, and the
//! book is binding policy here (`docs/src/concepts/checks-and-citations.md`):
//! when the band-gap refusal became a warning on 2026-08-27, two concept
//! pages went on stating the refusal as current design — a reader of the
//! documentation and a reader of the code got opposite answers for three
//! weeks, and the documentation is the one an operator reads first.
//!
//! # What is checked, and why this shape
//!
//! The guard is anchored on the identifier rather than on a phrase: every
//! paragraph of `docs/src/**/*.md` that names `erp_band_gap` — the check
//! whose refusal the policy superseded — must
//!
//! 1. use the policy's verb (`warn`), and
//! 2. carry no *affirmative* claim that something is refused; a negated
//!    mention ("never refused", "warned about rather than refused") is the
//!    whole point and stays admissible.
//!
//! A phrase-level "the docs must contain sentence X" guard would pin wording
//! rather than meaning and would be satisfied by pasting the sentence
//! anywhere. Anchoring on the identifier ties the claim to the thing it is a
//! claim about: a paragraph that explains this check has to explain it as
//! what it is.
//!
//! The coverage assertion at the end is the positive control: without it the
//! guard passes vacuously the day the last mention is renamed away, which is
//! exactly the day it stops protecting anything.

use std::fs;
use std::path::{Path, PathBuf};

/// The check whose behaviour the policy fixes. A paragraph naming it is
/// making a claim about a regulatory decision, whatever else it says.
const ANCHOR: &str = "erp_band_gap";

/// The verb the policy uses. Lower-cased comparison, so "Warn", "warned"
/// and "warning" all count.
const POLICY_VERB: &str = "warn";

/// Stem of every inflection of "refuse" (refuse/refuses/refused/refusal).
const REFUSAL_STEM: &str = "refus";

/// How far back of a refusal word a negation may sit and still govern it.
/// One clause, not one sentence: a negation two sentences earlier does not
/// make the claim beside it a negated one.
const NEGATION_WINDOW: usize = 80;

/// Words and phrases that turn a refusal word into a statement that there is
/// no refusal. Single words are matched on word boundaries so "another" is
/// not read as "not" and "nothing" is not read as "not"; the multi-word
/// forms are matched as substrings, where they cannot collide.
///
/// `no` is in the list even though it also opens affirmative sentences
/// ("no LoRa bandwidth fits, so the carrier is refused"), because the
/// policy's own heading is "No radio configuration is refused" and its
/// anchor slug repeats it in every link to it. The trade is deliberate and
/// in the safe direction: a guard that false-*negatives* on one phrasing
/// misses a line, a guard that false-positives on the correct text gets
/// switched off (`docs/src/concepts/checks-and-citations.md`).
const NEGATION_WORDS: &[&str] = &["no", "not", "never", "neither", "nor"];
const NEGATION_PHRASES: &[&str] = &["rather than", "instead of", "no longer"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

fn markdown_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
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
            // `book` is generated output; it would double every finding.
            if entry.file_name() == "book" {
                continue;
            }
            markdown_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// One blank-line-separated block of a markdown file, with the 1-based line
/// its first line sits on.
struct Paragraph {
    line: usize,
    text: String,
}

fn paragraphs(body: &str) -> Vec<Paragraph> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut start = 1usize;
    for (idx, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                out.push(Paragraph {
                    line: start,
                    text: current.join("\n"),
                });
                current.clear();
            }
        } else {
            if current.is_empty() {
                start = idx + 1;
            }
            current.push(line);
        }
    }
    if !current.is_empty() {
        out.push(Paragraph {
            line: start,
            text: current.join("\n"),
        });
    }
    out
}

/// Is `word` present in `haystack` as a whole word? `haystack` is already
/// lower-cased; anything that is not an ASCII letter is a boundary.
fn contains_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|token| token == word)
}

/// Is the refusal word at `at` governed by a negation in front of it?
fn is_negated(lower: &str, at: usize) -> bool {
    let from = lower[..at]
        .char_indices()
        .rev()
        .take(NEGATION_WINDOW)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let before = &lower[from..at];
    NEGATION_PHRASES.iter().any(|p| before.contains(p))
        || NEGATION_WORDS.iter().any(|w| contains_word(before, w))
}

/// Every affirmative refusal claim in a paragraph, as the snippet around it.
fn affirmative_refusals(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(REFUSAL_STEM) {
        let at = from + rel;
        if !is_negated(&lower, at) {
            let lo = text[..at]
                .char_indices()
                .rev()
                .take(60)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            let hi = text[at..]
                .char_indices()
                .take(40)
                .last()
                .map(|(i, c)| at + i + c.len_utf8())
                .unwrap_or(text.len());
            found.push(text[lo..hi].replace('\n', " "));
        }
        from = at + REFUSAL_STEM.len();
    }
    found
}

#[test]
fn the_book_describes_the_band_gap_as_a_warning_never_a_refusal() {
    let root = repo_root();
    let mut docs = Vec::new();
    markdown_under(&root.join("docs").join("src"), &mut docs);
    docs.sort();
    assert!(!docs.is_empty(), "docs/src carries markdown");

    let mut checked = 0usize;
    let mut problems: Vec<String> = Vec::new();

    for doc in &docs {
        let Ok(body) = fs::read_to_string(doc) else {
            continue;
        };
        let shown = doc.strip_prefix(&root).unwrap_or(doc).display().to_string();
        for para in paragraphs(&body) {
            if !para.text.contains(ANCHOR) {
                continue;
            }
            checked += 1;
            let lower = para.text.to_lowercase();
            if !lower.contains(POLICY_VERB) {
                problems.push(format!(
                    "{shown}:{} explains `{ANCHOR}` without saying it warns:\n    {}",
                    para.line,
                    para.text.replace('\n', "\n    ")
                ));
            }
            for snippet in affirmative_refusals(&para.text) {
                problems.push(format!(
                    "{shown}:{} states a regulatory refusal: \"...{snippet}...\"",
                    para.line
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "no radio configuration is refused for a regulatory reason (Codeberg #257); \
         the book still says otherwise in {} place(s):\n{}",
        problems.len(),
        problems.join("\n")
    );
    assert!(
        checked >= 2,
        "the guard checked {checked} paragraph(s) naming `{ANCHOR}`; \
         it is anchored on that name, so with fewer than the two known \
         mentions it is passing vacuously — re-anchor it or delete it"
    );
}
