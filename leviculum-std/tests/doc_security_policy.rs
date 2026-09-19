//! Doc guard: the repository must offer a private route for reporting a
//! security vulnerability, and that route must be an address somebody reads.
//!
//! Codeberg #289. Until this landed the tree had no `SECURITY.md` and no
//! statement anywhere about reporting, which left a finder two options:
//! open a public issue — which hands the vulnerability to everyone,
//! including whoever would use it — or say nothing. For a stack whose
//! headline property is end-to-end encryption that is the wrong pair of
//! options, and the absence is the kind that gets discovered by the first
//! person who has something to report.
//!
//! # What is checked, and why this shape
//!
//! Anchored on the contact and on structure rather than on sentences, for
//! the reason `doc_radio_policy.rs` gives: a "the doc must contain sentence
//! X" guard pins wording rather than meaning and is satisfied by pasting
//! the sentence anywhere.
//!
//! 1. **The policy exists at the repository root**, where Codeberg surfaces
//!    it in the sidebar the same way it surfaces the licence.
//! 2. **Every address it names is a declared project identity.** The list
//!    is parsed out of `scripts/commit-trailer-baseline.txt` — its `ours`
//!    lines, which that file documents as "who is inside the project", a
//!    declaration a reviewer has to read a diff to change. Reusing it is
//!    what keeps this guard from being a second list of addresses that
//!    drifts from the first. A `noreply` address is refused even when it is
//!    declared: it is an inbox nobody can answer from, and an unmonitored
//!    contact is worse than none because it still looks like a channel.
//! 3. **It answers the three questions a reporter has before they report**,
//!    each as its own section: where to send it, what happens then, and
//!    which versions are covered.
//! 4. **The windows are numbers.** The section that says what to expect
//!    must state at least two day counts — an acknowledgement window and a
//!    disclosure window. "As soon as possible" is not something a reporter
//!    deciding whether to wait or to publish can act on.
//! 5. **Coverage** (the positive control): the parsed identity list must be
//!    non-empty and must hold at least one answerable address, and the
//!    policy must name at least one. Without that, the day the parser stops
//!    matching is the day check 2 starts passing vacuously — on a policy
//!    with no contact in it at all, which is exactly the defect #289 is
//!    about.
//!
//! What it does NOT catch: whether the address is in fact read, or whether
//! the windows are honoured. Neither is reachable from a test. What is
//! reachable is that the document cannot name an address the project has
//! not declared as its own, and cannot quietly lose its numbers.

use std::fs;
use std::path::{Path, PathBuf};

/// The policy under guard.
const POLICY: &str = "SECURITY.md";

/// Where the project declares which identities are its own.
const IDENTITIES: &str = "scripts/commit-trailer-baseline.txt";

/// Section headings the policy must have, as a substring each heading is
/// matched by case-insensitively, with what it owes a reporter.
const SECTIONS: &[(&str, &str)] = &[
    ("report", "where to send a report, privately"),
    ("expect", "what happens after a report arrives"),
    ("supported versions", "which versions a fix reaches"),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} is not readable: {e}", path.display()))
}

/// Identities the project declares as its own: the `ours` lines of the
/// commit-trailer baseline.
fn declared_identities(baseline: &str) -> Vec<String> {
    baseline
        .lines()
        .filter_map(|line| line.strip_prefix("ours "))
        .map(|addr| addr.trim().to_ascii_lowercase())
        .collect()
}

/// An address is answerable when a reply to it reaches a person. A forge's
/// `noreply` stamp is a valid identity for authorship and a dead end for a
/// report.
fn answerable(addr: &str) -> bool {
    !addr.contains("noreply")
}

/// Every e-mail address in `text`, lower-cased. Hand-rolled rather than a
/// regex: the shapes that appear in Markdown are a bare address, one inside
/// a `mailto:` link, and one inside angle brackets, and all three fall out
/// of splitting on whitespace and Markdown punctuation.
fn addresses(text: &str) -> Vec<String> {
    const EDGE: &[char] = &[
        '<', '>', '(', ')', '[', ']', '`', '*', '"', '\'', ',', '.', ';', ':',
    ];
    let mut found = Vec::new();
    for token in text.split_whitespace() {
        let token = token.trim_matches(EDGE);
        let token = token.strip_prefix("mailto:").unwrap_or(token);
        let token = token.trim_matches(EDGE);
        let Some((local, domain)) = token.split_once('@') else {
            continue;
        };
        // A domain with no dot is not an address; `@handle` mentions and
        // `user@host`-style examples both land here.
        if local.is_empty() || !domain.contains('.') {
            continue;
        }
        found.push(token.to_ascii_lowercase());
    }
    found
}

/// The body of the section whose heading contains `needle`, up to the next
/// heading of the same or a higher level.
fn section_body<'a>(doc: &'a str, needle: &str) -> Option<&'a str> {
    let mut start = None;
    let mut level = 0usize;
    let mut end = doc.len();
    let mut offset = 0usize;
    for line in doc.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if hashes == 0 {
            continue;
        }
        if start.is_none() {
            if line.to_ascii_lowercase().contains(needle) {
                start = Some(offset);
                level = hashes;
            }
            continue;
        }
        if hashes <= level {
            end = line_start;
            break;
        }
    }
    start.map(|s| &doc[s..end])
}

/// Every "N days" / "N day" count in `text`.
fn day_counts(text: &str) -> Vec<u32> {
    let is_unit = |word: &str| {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphabetic());
        word.eq_ignore_ascii_case("day") || word.eq_ignore_ascii_case("days")
    };
    let as_count = |word: &str| {
        word.trim_matches(|c: char| !c.is_ascii_digit())
            .parse::<u32>()
            .ok()
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| is_unit(pair[1]))
        .filter_map(|pair| as_count(pair[0]))
        .collect()
}

#[test]
fn security_policy_exists_at_the_repository_root() {
    let path = repo_root().join(POLICY);
    assert!(
        path.is_file(),
        "{POLICY} is missing (Codeberg #289). Without it a finder's only \
         public-facing option is an issue, which discloses the vulnerability \
         to everyone at once."
    );
}

#[test]
fn security_policy_names_a_contact_the_project_has_declared_as_its_own() {
    let declared = declared_identities(&read(IDENTITIES));

    // Positive control for the parser and for the list it reads: an empty
    // or all-`noreply` result would make the assertion below vacuous.
    assert!(
        !declared.is_empty(),
        "no `ours` line parsed out of {IDENTITIES}; the contact check below \
         would pass on any address at all"
    );
    assert!(
        declared.iter().any(|a| answerable(a)),
        "every declared identity in {IDENTITIES} is a noreply address, so \
         there is no address this guard could accept"
    );

    let policy = read(POLICY);
    let named = addresses(&policy);
    assert!(
        !named.is_empty(),
        "{POLICY} names no e-mail address. A policy without a contact route \
         is the defect Codeberg #289 is about, not a fix for it."
    );
    for addr in &named {
        assert!(
            declared.contains(addr),
            "{POLICY} names <{addr}>, which is not one of the identities \
             {IDENTITIES} declares as ours ({declared:?}). Either the \
             address is wrong, or the project has a new identity and that \
             file is where it gets declared."
        );
        assert!(
            answerable(addr),
            "{POLICY} names <{addr}>: a noreply address cannot receive a \
             report. An unmonitored contact is worse than none, because it \
             still looks like a channel."
        );
    }
}

#[test]
fn security_policy_answers_what_a_reporter_needs_before_reporting() {
    let policy = read(POLICY).to_ascii_lowercase();
    for (needle, owes) in SECTIONS {
        assert!(
            policy
                .lines()
                .any(|line| line.starts_with('#') && line.contains(needle)),
            "{POLICY} has no section whose heading names {needle:?}: {owes}"
        );
    }
}

#[test]
fn security_policy_states_its_windows_as_numbers() {
    let policy = read(POLICY);
    let body = section_body(&policy.to_ascii_lowercase(), "expect")
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{POLICY} has no section about what to expect"));
    let days = day_counts(&body);
    assert!(
        days.len() >= 2,
        "the expectations section of {POLICY} states {} day count(s) ({days:?}); \
         a reporter deciding whether to wait or to publish needs both an \
         acknowledgement window and a disclosure window as numbers",
        days.len()
    );
}
