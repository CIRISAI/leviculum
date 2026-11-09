// Compose the full version string for the lnpnd binary, the way
// leviculum-cli/build.rs does for lnsd: the crate version, the nightly
// build id from LEVICULUM_BUILD_ID when CI sets it, `+heapacct` in an
// instrumented build, `+mimalloc` on the default allocator, and the git HEAD hash in parentheses, e.g.
// "0.1.0-nightly.20261025-a3d7958+heapacct (a3d7958f…)". The hash is what
// lets a deploy prove from the binary which commit it runs (1000); before
// it, `lnpnd --version` printed the bare crate version on every nightly.
// The result lands in LEVICULUM_VERSION, which main.rs hands to clap.
// Falls back to "unknown" without git.
fn main() {
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let build_id = std::env::var("LEVICULUM_BUILD_ID").unwrap_or_default();
    let base = if build_id.is_empty() {
        pkg_version
    } else {
        format!("{pkg_version}-{build_id}")
    };
    // Before the hash, as lnsd's, so the parenthesised-hash parse is
    // untouched: the soak node's journal and `dpkg -l` must not be the only
    // places that tell the instrumented daemon from the plain one (550).
    // Cargo sets the variable for every enabled feature, so this needs no
    // rerun-if line.
    let base = if std::env::var_os("CARGO_FEATURE_HEAP_ACCOUNTING").is_some() {
        format!("{base}+heapacct")
    } else {
        base
    };
    // Same rule for the allocator (1592, as lnsd's 508): a mimalloc lnpnd on
    // the soak node has to be told apart from a mallocng one by `--version`
    // alone.
    let base = if std::env::var_os("CARGO_FEATURE_ALLOC_MIMALLOC").is_some() {
        format!("{base}+mimalloc")
    } else {
        base
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
