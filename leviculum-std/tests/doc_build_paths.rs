//! Build-path guard: a document that tells a stranger where the binaries
//! land must name the directory cargo actually writes them to.
//!
//! `.cargo/config.toml` pins `[build] target`. With that set, cargo places
//! every host artefact under `target/<triple>/release/` and never creates
//! `target/release/` as an artefact directory at all — it exists, but holds
//! only `build/`, `deps/`, `examples/` and `incremental/`. Both public
//! source-build documents nevertheless named `./target/release/lnsd` until
//! 2026-09-02 (Codeberg #265), so a stranger's first hour ended in
//! "No such file or directory" one line after a successful build, with no
//! correct path stated anywhere to fall back on.
//!
//! The documents were corrected by hand. Nothing kept them corrected: the
//! triple lives in `.cargo/config.toml` and the docs repeat it as a literal,
//! which is drift waiting to happen the next time the pinned target moves.
//! This guard makes `.cargo/config.toml` the single source of truth, the
//! same one `scripts/lnflash-bundle.sh` already builds against.
//!
//! Two checks, because the old text failed in two different ways:
//!
//! 1. **Every documented invocation resolves.** A path of the shape
//!    `target/[<triple>/]release/<binary>` naming one of this workspace's
//!    host binaries must carry the configured triple, or one of the host
//!    overrides the same documents teach (`HOST_OVERRIDE_TRIPLES`). The
//!    old README's `./target/release/lnsd -v` had no triple at all.
//! 2. **Every source-build document names the real directory.** The old
//!    installation guide said "The binaries are in `target/release/`" —
//!    a bare directory with no binary after it, so check 1 cannot see it.
//!    Requiring the configured `target/<triple>/release/` to appear at
//!    least once catches the claim made as prose rather than as a command.
//!
//! Check 1 runs over every host document; check 2 only over the two
//! documents that teach a source build, because only those owe the reader
//! a path.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

/// The documents that teach a stranger to build from source, and therefore
/// owe them the directory the build writes to.
const SOURCE_BUILD_DOCS: &[&str] = &["README.md", "docs/src/guide/installation.md"];

/// This workspace's host binaries. A `target/.../release/<name>` path is only
/// a claim about *our* build output when `<name>` is one of these: the
/// firmware docs legitimately name `target/thumbv7em-none-eabihf/release/t114`
/// (a different workspace, a different target) and the rig scripts name
/// periculum's own `target/release/periculum` (a different repository).
const HOST_BINS: &[&str] = &[
    "lnsd", "lnstest", "lncp", "lnstatus", "lnprobe", "lnpath", "lntd", "lnmsg", "lnomad", "lnpnd",
    "lblogd", "lndecode", "lnflash",
];

/// Triples a document may name besides the pin, because we tell readers to
/// build with them: the pin is unconditional and wrong on any host that is
/// not x86_64, so the source-build documents carry an arm64 override
/// (Codeberg #291, guarded by `build_target_host_arch.rs`). A path under
/// one of these is a documented build, not stale text.
const HOST_OVERRIDE_TRIPLES: &[&str] = &["aarch64-unknown-linux-musl"];

/// Never walked: `docs/book` is generated output that would double every
/// finding, `target` is build output, `reference` is a foreign submodule.
const SKIP_DIRS: &[&str] = &[".git", "target", "book", "node_modules", "reference"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

/// The triple `.cargo/config.toml` pins, or `None` if `[build] target` is
/// not set. Parsed by hand rather than with a TOML crate so the test needs
/// no dependency the workspace does not already carry for other reasons.
fn configured_target(config: &str) -> Option<String> {
    let mut in_build = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_build = line == "[build]";
            continue;
        }
        if !in_build {
            continue;
        }
        let Some(value) = line.strip_prefix("target") else {
            continue;
        };
        let Some(value) = value.trim_start().strip_prefix('=') else {
            continue;
        };
        return Some(value.trim().trim_matches('"').to_string());
    }
    None
}

/// A path naming a host binary inside a cargo artefact directory. The triple
/// segment is optional so the shape that has *no* triple — the bug this
/// guards — is matched rather than skipped.
fn artefact_path_re() -> Regex {
    Regex::new(r"target/(?:([A-Za-z0-9_.-]+)/)?release/([A-Za-z0-9_-]+)")
        .expect("artefact path pattern compiles")
}

/// Check 1 over one document's text. Returns one message per bad path.
///
/// `want` is the pinned triple first, then any triple a document may name
/// because we document building with it. The pin leads because it is what
/// the messages quote.
fn bad_artefact_paths(text: &str, want: &[&str]) -> Vec<String> {
    let want_triple = want.first().copied().unwrap_or_default();
    let re = artefact_path_re();
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        for caps in re.captures_iter(line) {
            let bin = caps.get(2).map_or("", |m| m.as_str());
            if !HOST_BINS.contains(&bin) {
                continue;
            }
            match caps.get(1) {
                None => out.push(format!(
                    "line {}: `{}` — the workspace pins `[build] target = \"{}\"`, so \
                     cargo writes `{}` to `target/{}/release/{}` and `target/release/` \
                     never holds a binary at all",
                    idx + 1,
                    caps.get(0).map_or("", |m| m.as_str()),
                    want_triple,
                    bin,
                    want_triple,
                    bin,
                )),
                Some(triple) if !want.contains(&triple.as_str()) => out.push(format!(
                    "line {}: `{}` — names target `{}`, but `.cargo/config.toml` pins \
                     `{}`",
                    idx + 1,
                    caps.get(0).map_or("", |m| m.as_str()),
                    triple.as_str(),
                    want_triple,
                )),
                Some(_) => {}
            }
        }
    }
    out
}

/// Check 2 over one document's text: does it name the artefact directory
/// the pinned target produces, anywhere at all?
fn names_artefact_dir(text: &str, want_triple: &str) -> bool {
    text.contains(&format!("target/{want_triple}/release/"))
}

fn walk_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
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
            let name = entry.file_name();
            if SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            walk_markdown(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

fn pinned_triple(root: &Path) -> String {
    let config = fs::read_to_string(root.join(".cargo/config.toml"))
        .expect("the workspace has a .cargo/config.toml");
    configured_target(&config).expect(
        "`.cargo/config.toml` sets `[build] target`; if that pin was removed on purpose, \
         this guard and the two source-build documents are what needs revisiting",
    )
}

#[test]
fn documented_binary_paths_use_the_pinned_target() {
    let root = repo_root();
    let triple = pinned_triple(&root);
    let accepted: Vec<&str> = std::iter::once(triple.as_str())
        .chain(HOST_OVERRIDE_TRIPLES.iter().copied())
        .collect();

    let mut docs = Vec::new();
    walk_markdown(&root, &mut docs);
    docs.sort();
    assert!(
        docs.len() > 20,
        "expected the markdown corpus, found {} files — the walk is broken, not the docs",
        docs.len()
    );

    let mut failures = Vec::new();
    for doc in &docs {
        let Ok(text) = fs::read_to_string(doc) else {
            continue;
        };
        let rel = doc.strip_prefix(&root).unwrap_or(doc);
        for msg in bad_artefact_paths(&text, &accepted) {
            failures.push(format!("{}: {msg}", rel.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "documents name a build path cargo never writes:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn source_build_docs_name_the_artefact_directory() {
    let root = repo_root();
    let triple = pinned_triple(&root);

    let mut missing = Vec::new();
    for doc in SOURCE_BUILD_DOCS {
        let text = fs::read_to_string(root.join(doc))
            .unwrap_or_else(|e| panic!("{doc} is a source-build document and must exist: {e}"));
        if !names_artefact_dir(&text, &triple) {
            missing.push(*doc);
        }
    }

    assert!(
        missing.is_empty(),
        "these documents teach a source build without ever naming \
         `target/{triple}/release/`, so the reader is left to guess: {}",
        missing.join(", ")
    );
}

/// Positive control: the text that shipped at master 752baa42, verbatim. A
/// guard that has never been red proves nothing, and this is the failure it
/// exists for — both halves of it, since the two documents got the path
/// wrong in two shapes.
#[test]
fn the_original_text_is_caught() {
    let triple = "x86_64-unknown-linux-musl";

    // README.md at 752baa42: the verification command after the build.
    let old_readme = "cargo build --release --bin lnsd --bin lnstatus --bin lncp --bin lnstest\n\
                      ./target/release/lnsd -v\n";
    let found = bad_artefact_paths(old_readme, &[triple]);
    assert_eq!(
        found.len(),
        1,
        "the README's `./target/release/lnsd -v` must be reported: {found:?}"
    );
    assert!(!names_artefact_dir(old_readme, triple));

    // docs/src/guide/installation.md at 752baa42: stated as prose, with no
    // binary after the directory, which is why check 2 has to exist.
    let old_guide = "The binaries are in `target/release/`.\n";
    assert!(
        bad_artefact_paths(old_guide, &[triple]).is_empty(),
        "no binary is named, so check 1 cannot and should not fire here"
    );
    assert!(
        !names_artefact_dir(old_guide, triple),
        "check 2 is what catches the directory claimed as prose"
    );

    // The corrected text passes both.
    let fixed = "so the binaries land under `target/x86_64-unknown-linux-musl/release/`, \
                 not `target/release/`.\n./target/x86_64-unknown-linux-musl/release/lnsd --version\n";
    assert!(bad_artefact_paths(fixed, &[triple]).is_empty());
    assert!(names_artefact_dir(fixed, triple));
}

/// A moved pin must turn the documents red rather than silently disagree
/// with them: that is the whole point of reading the triple from config
/// instead of hardcoding it here.
#[test]
fn a_moved_pin_reports_the_stale_triple() {
    let current = "./target/x86_64-unknown-linux-musl/release/lnsd --version\n";
    let found = bad_artefact_paths(current, &["aarch64-unknown-linux-musl"]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("pins"), "{found:?}");
}

#[test]
fn foreign_target_directories_are_not_our_claim() {
    let triple = "x86_64-unknown-linux-musl";
    // Firmware: a different workspace and a different target.
    assert!(bad_artefact_paths(
        "sudo cp target/thumbv7em-none-eabihf/release/t114.uf2 /mnt/NEW.UF2\n",
        &[triple]
    )
    .is_empty());
    // periculum: a different repository entirely.
    assert!(bad_artefact_paths(
        "PERICULUM_BIN=\"$PERICULUM_ROOT/target/release/periculum\"\n",
        &[triple]
    )
    .is_empty());
}

/// A path under a documented host override is a build we tell people to
/// run, not drift. A triple we document nowhere still is drift.
#[test]
fn a_documented_host_override_is_not_stale_text() {
    let accepted = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];
    assert!(bad_artefact_paths(
        "./target/aarch64-unknown-linux-musl/release/lnsd --version\n",
        &accepted
    )
    .is_empty());
    let found = bad_artefact_paths(
        "./target/armv7-unknown-linux-musleabihf/release/lnsd --version\n",
        &accepted,
    );
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn the_build_target_is_read_out_of_the_config_section() {
    let config = "[alias]\ntarget = \"not-this-one\"\n\n[build]\ntarget = \"the-real-one\"\n";
    assert_eq!(configured_target(config).as_deref(), Some("the-real-one"));
    assert_eq!(configured_target("[build]\nrustflags = []\n"), None);
}
