// Compose the full version string for the loquax binary, mirroring
// leviculum-cli/build.rs. Local builds get the plain crate version
// ("0.7.0"). CI nightly builds get a suffix via the LEVICULUM_BUILD_ID
// env var, producing something like "0.7.0-nightly.20260419-5a5df20".
// The result lands in env var LEVICULUM_VERSION, which main.rs picks up
// with env!() and hands to clap's #[command(version = …)] attribute.
//
// The current git HEAD hash is always appended in parentheses, e.g.
// "0.7.0 (1a2b3c…)", and also exposed raw via LEVICULUM_GIT_HASH. The
// integ harness parses the parenthesised hash out of `<bin> --version`
// and refuses to run a binary whose hash does not match the repo HEAD,
// catching a wrong-branch binary that mtime alone cannot (its mtime is
// newer than the current commit). Falls back to "unknown" without git.
//
// A tree with changes not yet committed builds as "<hash>-dirty" (1647):
// the 1642 APK was built before its own commits and named the commit
// before them, which then read as the build of that commit on the mailbox
// page. The stamp names what it was built from or says that it was not a
// commit. Git's own files are found through `git rev-parse` (a worktree's
// `.git` is a file), and the package's sources are watched too, so an
// edit or a commit reruns this script.
fn main() {
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
    // Rebuild when HEAD moves (checkout, commit) or the tree changes, so
    // the embedded hash and its mark track the working tree.
    for path in git_watched() {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
}

/// The output of `git <args>`, trimmed, or `None`.
fn git(args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Git's files that move with a checkout, a commit or a staged change:
/// HEAD and the index of this tree's git dir, and the ref of the branch
/// checked out (only that one: other worktrees of the same repository
/// commit to theirs, which must not rebuild this one).
fn git_watched() -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        paths.push(format!("{dir}/HEAD"));
        paths.push(format!("{dir}/index"));
    }
    if let Some(common) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
            paths.push(format!("{common}/{branch}"));
        }
        paths.push(format!("{common}/packed-refs"));
    }
    paths
}

/// Resolve the current git HEAD commit hash, or "unknown" if git is
/// unavailable (no repo, no git binary, detached state without commits).
fn git_head_hash() -> String {
    let Some(hash) = git(&["rev-parse", "HEAD"]) else {
        return "unknown".to_string();
    };
    // Tracked files changed against HEAD: not a build of that commit.
    let clean = std::process::Command::new("git")
        .args(["diff", "--quiet", "HEAD", "--"])
        .status()
        .map(|s| s.success())
        .unwrap_or(true);
    match clean {
        true => hash,
        false => format!("{hash}-dirty"),
    }
}
