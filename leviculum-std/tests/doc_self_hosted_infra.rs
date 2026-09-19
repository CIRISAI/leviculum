//! Doc guard: the self-hosted-infrastructure design record must name the
//! parts of itself that have since been built.
//!
//! `docs/src/concepts/self-hosted-infrastructure.md` (Codeberg #260) is a
//! design record for a second, self-controlled home for the project. It
//! opened by saying that none of it existed yet, and that was true when it
//! was written on 2026-08-17. It stopped being true on 2026-09-18, when the
//! clearweb download half landed — a sender (`scripts/publish-site.sh`), a
//! receiver (`packaging/site/lev-receive-nightly`) and the pipeline step
//! that runs them (`.woodpecker/nightly.yml`) — under a different issue and
//! without the record noticing. A design record that claims nothing of it is
//! built sends the next reader to build what is already in the tree.
//!
//! # What is checked, and why this shape
//!
//! Anchored on the artifacts rather than on a sentence, for the reason
//! `doc_radio_policy.rs` gives: a "the doc must contain sentence X" guard
//! pins wording rather than meaning and is satisfied by pasting the sentence
//! anywhere.
//!
//! 1. **Every landed artifact that exists in this tree is named in the
//!    record, by its repo-relative path.** The existence test is what makes
//!    the list self-maintaining in the safe direction: an artifact that is
//!    deleted or renamed stops being required, so the guard does not hold a
//!    doc to a file nobody has any more.
//! 2. **The releases root the record describes is the one the receiver
//!    actually defaults to.** The value is parsed out of
//!    `lev-receive-nightly` at test time rather than written down here, so a
//!    change to the script's default is a red test and not a second copy
//!    that drifts.
//! 3. **Coverage** (the positive control): at least one landed artifact must
//!    exist. Without it, the day the last one is renamed away this guard
//!    starts passing vacuously — which is the day it stops protecting
//!    anything.
//!
//! What it does NOT catch: prose elsewhere in the record that contradicts
//! what it names here. A guard that tried to read the claim would be a
//! phrase guard. Naming the artifact is what puts the built thing in front
//! of the next editor's eyes.

use std::fs;
use std::path::{Path, PathBuf};

/// The design record under guard.
const RECORD: &str = "docs/src/concepts/self-hosted-infrastructure.md";

/// Parts of the design record that exist in this tree. Each entry is a
/// repo-relative path; the second field says which component of the record
/// it belongs to, and appears only in the failure message, to tell whoever
/// hits this where in the record the artifact belongs.
const LANDED: &[(&str, &str)] = &[
    (
        "scripts/publish-site.sh",
        "Component 4 — the sender that hands a nightly to the site",
    ),
    (
        "packaging/site/lev-receive-nightly",
        "Component 3 step 4 / Component 4 — the receiver that publishes it \
         add-then-swap and keeps the previous builds",
    ),
    (
        ".woodpecker/nightly.yml",
        "Component 3 — the nightly that runs both, gated on a green test step",
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

/// The `RELEASES_ROOT` default from the receiver, i.e. the line
/// `RELEASES_ROOT="${RELEASES_ROOT:-/var/www/leviculum/releases}"`.
fn receiver_releases_root(script: &str) -> Option<String> {
    let marker = "RELEASES_ROOT:-";
    for line in script.lines() {
        // The comment that documents the default names it too; take the
        // assignment, which is the one the script runs.
        if line.trim_start().starts_with('#') {
            continue;
        }
        let Some(rest) = line.split_once(marker).map(|(_, r)| r) else {
            continue;
        };
        let Some(value) = rest.split_once('}').map(|(v, _)| v) else {
            continue;
        };
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

#[test]
fn the_design_record_names_the_parts_of_itself_that_are_built() {
    let root = repo_root();
    let record_path = root.join(RECORD);
    let record = fs::read_to_string(&record_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", record_path.display()));

    let mut present = 0usize;
    let mut missing: Vec<String> = Vec::new();

    for (path, component) in LANDED {
        if !root.join(path).exists() {
            continue;
        }
        present += 1;
        if !record.contains(path) {
            missing.push(format!("  {path}\n    ({component})"));
        }
    }

    assert!(
        present > 0,
        "none of the landed artifacts this guard knows about exists any more, \
         so it now checks nothing. Either the list in LANDED is stale (update \
         it with what the self-hosted infrastructure work actually built) or \
         {RECORD} no longer describes anything that is in this tree."
    );

    assert!(
        missing.is_empty(),
        "{RECORD} does not name these parts of itself, which are built and \
         in this tree:\n{}\n\nThe record is read as the statement of what \
         does not exist yet. Name each of them where it belongs, so the next \
         reader does not build it a second time.",
        missing.join("\n")
    );
}

#[test]
fn the_design_record_describes_the_releases_root_the_receiver_uses() {
    let root = repo_root();
    let receiver_path = root.join("packaging/site/lev-receive-nightly");
    if !receiver_path.exists() {
        // Guarded by the coverage assertion in the test above.
        return;
    }

    let receiver = fs::read_to_string(&receiver_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", receiver_path.display()));
    let releases_root = receiver_releases_root(&receiver).expect(
        "no RELEASES_ROOT default found in packaging/site/lev-receive-nightly; \
         the guard parses the assignment `RELEASES_ROOT=\"${RELEASES_ROOT:-...}\"` \
         and that line has moved",
    );

    let record_path = root.join(RECORD);
    let record = fs::read_to_string(&record_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", record_path.display()));

    assert!(
        record.contains(&releases_root),
        "{RECORD} does not name '{releases_root}', which is where \
         packaging/site/lev-receive-nightly publishes by default. A record \
         that names a different directory than the script sends whoever \
         provisions the server to serve an empty one."
    );
}
