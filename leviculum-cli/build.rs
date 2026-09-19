// Compose a full version string for the three CLI binaries (lnsd, lnstest,
// lncp). Local builds get the plain crate version ("0.7.0"). CI nightly
// builds get a suffix via the LEVICULUM_BUILD_ID env var, producing
// something like "0.7.0-nightly.20260419-5a5df20". The result lands in
// env var LEVICULUM_VERSION, which the binaries pick up with env!() and
// hand to clap's #[command(version = …)] attribute.
//
// The current git HEAD hash is always appended in parentheses, e.g.
// "0.7.0 (1a2b3c…)", and also exposed raw via LEVICULUM_GIT_HASH. The
// integ harness parses the parenthesised hash out of `<bin> --version`
// and refuses to run a binary whose hash does not match the repo HEAD,
// catching a wrong-branch binary that mtime alone cannot (its mtime is
// newer than the current commit). Falls back to "unknown" without git.
// Shared with leviculum-std/tests/build_target_host_arch.rs, which is
// where the detection below is tested: build scripts are not compiled as
// test targets.
include!("build/host_target.rs");

fn main() {
    warn_on_foreign_default_target();

    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let build_id = std::env::var("LEVICULUM_BUILD_ID").unwrap_or_default();
    let base = if build_id.is_empty() {
        pkg_version
    } else {
        format!("{pkg_version}-{build_id}")
    };
    let git_hash = git_head_hash();
    let full = format!("{base} ({git_hash})");
    println!("cargo:rustc-env=LEVICULUM_VERSION={full}");
    println!("cargo:rustc-env=LEVICULUM_GIT_HASH={git_hash}");
    println!("cargo:rerun-if-env-changed=LEVICULUM_BUILD_ID");
    // Rebuild when HEAD moves (checkout, commit) so the embedded hash
    // tracks the working tree. Paths are relative to the package root,
    // and the workspace .git lives one level up.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
}

/// Say so when this build is producing binaries the machine running it
/// cannot execute, which is what the unconditional `[build] target` in
/// `.cargo/config.toml` does on any host that is not x86_64 (Codeberg
/// #291). The build succeeds either way — that is the point, the failure
/// used to appear much later as a bare "cannot execute binary file" — so
/// this is a warning, not an error, and it is silent for every target
/// that was asked for explicitly.
fn warn_on_foreign_default_target() {
    // Set for build scripts by cargo, always, with no config fallback.
    println!("cargo:rerun-if-env-changed=CARGO_BUILD_TARGET");
    println!("cargo:rerun-if-changed=build/host_target.rs");
    println!("cargo:rerun-if-changed=../.cargo/config.toml");
    let (Ok(host), Ok(target)) = (std::env::var("HOST"), std::env::var("TARGET")) else {
        return;
    };
    let pinned = std::fs::read_to_string("../.cargo/config.toml")
        .ok()
        .and_then(|config| configured_build_target(&config));
    if let Some(message) = foreign_default_target(&host, &target, pinned.as_deref()) {
        println!("cargo:warning={message}");
    }
}

/// Resolve the current git HEAD commit hash, or "unknown" if git is
/// unavailable (no repo, no git binary, detached state without commits).
fn git_head_hash() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
