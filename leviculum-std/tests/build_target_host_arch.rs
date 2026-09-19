//! Host-architecture guard for the workspace build target.
//!
//! `.cargo/config.toml` pins `[build] target = "x86_64-unknown-linux-musl"`
//! for every host. Cargo has no per-host conditional there, so on an arm64
//! machine — a Raspberry Pi, the most natural place to run a Reticulum node
//! — a fresh clone builds x86_64 binaries. They build successfully and then
//! do not run, and the execve failure says nothing about architecture
//! (Codeberg #291). The `.deb` route works; the documented build-from-source
//! route was the one that failed.
//!
//! The pin stays: musl-static is why a binary built here runs on any Linux,
//! and the same wrong answer would appear mirrored if the pin merely moved
//! to arm64. What changes is that the wrong answer is no longer silent and
//! the right one no longer undiscoverable. This guard holds both halves:
//!
//! 1. The detection `leviculum-cli/build.rs` warns from, tested here
//!    because a build script is not compiled as a test target — logic that
//!    lives only in one is logic nobody checks.
//! 2. The two documents that teach a source build name the override, so a
//!    reader on a foreign host can find it without reading a build script.
//!
//! Sibling guard: `doc_build_paths.rs`, which holds the same documents to
//! the artefact directory the pin produces.

use std::fs;
use std::path::{Path, PathBuf};

include!("../../leviculum-cli/build/host_target.rs");

/// The documents that teach a stranger to build from source, and therefore
/// owe a reader on a non-x86_64 host the one line that makes it work.
const SOURCE_BUILD_DOCS: &[&str] = &["README.md", "docs/src/guide/installation.md"];

/// The host override the documents are required to name. Not derived from
/// anything: arm64 is the audience the issue was filed for, and the
/// packaging already ships an `arm64` .deb for it.
const DOCUMENTED_OVERRIDE: &str = "aarch64-unknown-linux-musl";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel} must be readable: {e}"))
}

/// Does this text hand a foreign host a recipe it can follow — the
/// environment variable *and* the triple to give it? Either alone is half
/// an instruction.
fn documents_the_host_override(text: &str) -> bool {
    text.contains("CARGO_BUILD_TARGET") && text.contains(DOCUMENTED_OVERRIDE)
}

/// Positive control, and the reproduction: the configuration and the host
/// the issue was filed about. Nothing in the tree before this change
/// reported this case, which is the whole bug — the build succeeds.
#[test]
fn an_arm64_host_building_the_pinned_x86_64_target_is_reported() {
    let pinned = "x86_64-unknown-linux-musl";
    let msg = foreign_default_target("aarch64-unknown-linux-gnu", pinned, Some(pinned))
        .expect("an aarch64 host building the x86_64 pin must be reported");
    assert!(
        msg.contains("aarch64-unknown-linux-musl"),
        "the message must name the triple to build instead: {msg}"
    );
    assert!(
        msg.contains("CARGO_BUILD_TARGET"),
        "the message must name how to pass it: {msg}"
    );
}

/// The mirror image: moving the pin to arm64 would not fix anything, it
/// would move the failure to the other host. The guard has to fire on
/// whichever host is the odd one out, not on a hardcoded architecture.
#[test]
fn the_failure_is_symmetric_in_the_pin() {
    let pinned = "aarch64-unknown-linux-musl";
    let msg = foreign_default_target("x86_64-unknown-linux-gnu", pinned, Some(pinned))
        .expect("an x86_64 host building an arm64 pin must be reported too");
    assert!(msg.contains("x86_64-unknown-linux-musl"), "{msg}");
}

/// Every route that names a target on purpose stays quiet. A guard that
/// fires on `scripts/build-deb.sh arm64` — which cross-builds exactly the
/// triple this one recommends — would be noise in the nightly, and noise
/// in a nightly is how a real warning gets skipped.
#[test]
fn deliberate_cross_builds_are_not_reported() {
    let pinned = Some("x86_64-unknown-linux-musl");
    let host = "x86_64-unknown-linux-gnu";
    for target in [
        // scripts/build-deb.sh arm64
        "aarch64-unknown-linux-musl",
        // cargo build-ffi-arm64
        "aarch64-unknown-linux-gnu",
        // just check-nrf52 / check-rp2040
        "thumbv7em-none-eabihf",
        "thumbv6m-none-eabi",
        // just test-i686-usize
        "i686-unknown-linux-musl",
    ] {
        assert!(
            foreign_default_target(host, target, pinned).is_none(),
            "{target} was asked for explicitly and must not warn"
        );
    }
    // The host that followed the documentation: CARGO_BUILD_TARGET makes
    // TARGET differ from the pin, so the warning it prompted stops.
    assert!(foreign_default_target(
        "aarch64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        pinned
    )
    .is_none());
    // And the ordinary case this guard must never touch.
    assert!(foreign_default_target(host, "x86_64-unknown-linux-musl", pinned).is_none());
    // No pin at all: nothing to be wrong about.
    assert!(foreign_default_target(
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        None
    )
    .is_none());
}

#[test]
fn the_suggested_triple_is_the_musl_sibling_of_the_host() {
    assert_eq!(
        musl_variant("aarch64-unknown-linux-gnu"),
        "aarch64-unknown-linux-musl"
    );
    assert_eq!(
        musl_variant("riscv64gc-unknown-linux-gnu"),
        "riscv64gc-unknown-linux-musl"
    );
    // Already musl, or something this function has no business rewriting.
    assert_eq!(
        musl_variant("aarch64-unknown-linux-musl"),
        "aarch64-unknown-linux-musl"
    );
    assert_eq!(musl_variant("x86_64-apple-darwin"), "x86_64-apple-darwin");
    assert_eq!(triple_arch("aarch64-unknown-linux-gnu"), "aarch64");
    assert_eq!(triple_arch("nonsense"), "nonsense");
}

/// The pin this guard is about is the one in the tree, read the same way
/// the build script reads it.
#[test]
fn the_workspace_still_pins_one_target_for_every_host() {
    let root = repo_root();
    let pinned = configured_build_target(&read(&root, ".cargo/config.toml")).expect(
        "`.cargo/config.toml` sets `[build] target`; if that pin was dropped on purpose, \
         this guard, `doc_build_paths.rs` and the source-build documents are what needs \
         revisiting",
    );
    assert!(
        foreign_default_target("aarch64-unknown-linux-gnu", &pinned, Some(&pinned)).is_some(),
        "the pinned target {pinned} is still foreign to an arm64 host, so the guard must fire"
    );
}

/// The override has to work out of the box once someone passes it: the
/// documented triple gets the same rust-lld + self-contained linking the
/// pinned one does, so an arm64 host needs no musl toolchain either.
#[test]
fn the_documented_override_has_a_linker_section() {
    let config = read(&repo_root(), ".cargo/config.toml");
    assert!(
        config.contains(&format!("[target.{DOCUMENTED_OVERRIDE}]")),
        "`.cargo/config.toml` documents {DOCUMENTED_OVERRIDE} as the arm64 override but \
         configures no linker for it"
    );
}

/// Check 2: a reader on a Pi has to be able to find the override without
/// reading a build script or a test.
#[test]
fn source_build_docs_name_the_host_override() {
    let root = repo_root();
    let mut missing = Vec::new();
    for doc in SOURCE_BUILD_DOCS {
        if !documents_the_host_override(&read(&root, doc)) {
            missing.push(*doc);
        }
    }
    assert!(
        missing.is_empty(),
        "these documents teach a source build without telling a non-x86_64 host to pass \
         `CARGO_BUILD_TARGET={DOCUMENTED_OVERRIDE}`, so an arm64 reader builds binaries \
         that cannot run: {}",
        missing.join(", ")
    );
}

/// Positive control for check 2: the text that shipped at master 752baa42.
#[test]
fn the_original_documentation_is_caught() {
    let old_readme = "The workspace pins `x86_64-unknown-linux-musl` as its build target (see\n\
                      `.cargo/config.toml` for why), so the binaries land under\n\
                      `target/x86_64-unknown-linux-musl/release/`, not `target/release/`.\n";
    assert!(
        !documents_the_host_override(old_readme),
        "the old text names the pin and never says what a foreign host does with it"
    );
    // Naming the triple without the variable, or the variable without the
    // triple, is half a recipe and does not count.
    assert!(!documents_the_host_override(
        "On arm64 build for aarch64-unknown-linux-musl instead.\n"
    ));
    assert!(!documents_the_host_override(
        "Set CARGO_BUILD_TARGET to your host's musl triple.\n"
    ));
    assert!(documents_the_host_override(
        "CARGO_BUILD_TARGET=aarch64-unknown-linux-musl cargo build --release\n"
    ));
}

/// The detection is only worth anything if the build script still calls
/// it. Cheap textual check rather than a build fixture: the alternative
/// is running cargo inside a test, which this suite does not do.
#[test]
fn the_build_script_still_uses_the_detection() {
    let build_rs = read(&repo_root(), "leviculum-cli/build.rs");
    assert!(
        build_rs.contains("host_target.rs") && build_rs.contains("foreign_default_target"),
        "leviculum-cli/build.rs no longer includes or calls the host-architecture \
         detection, so nothing reports the pin on a foreign host"
    );
}
