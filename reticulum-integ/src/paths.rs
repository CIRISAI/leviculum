//! Binary path resolution and freshness checks for the integ runner.
//!
//! Two concerns, paired here because they share the same input
//! (`CARGO_TARGET_DIR`) and the same failure mode (running against stale
//! binaries):
//!
//! 1. `target_dir` / `release_bin` resolve the production-binary mount paths
//!    the way cargo itself does — honour `CARGO_TARGET_DIR`, fall back to
//!    `{repo_root}/target`. The nightly CI sets this env var, so without the
//!    resolver the runner was mounting stale binaries from `target/release/`
//!    while cargo built fresh ones into the cache directory.
//! 2. `check_binary_freshness` asserts that every binary the runner is about
//!    to mount was built from a commit at least as new as the current
//!    `HEAD`. A Nightly run that somehow skipped the rebuild step fails loud
//!    here instead of silently testing pre-parity code.
//!
//! The freshness check is opt-out via `LEVICULUM_SKIP_FRESHNESS_CHECK=1` so
//! local iteration (edit core, run one scenario) does not demand a full
//! rebuild. Nightly keeps it on.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

/// Returns the cargo target directory: `$CARGO_TARGET_DIR` if set, else
/// `{repo_root}/target`.
pub fn target_dir(repo_root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.join("target"))
}

/// Absolute path to a release binary under the resolved target dir.
///
/// Cargo writes release artefacts to `<target_dir>/release/` when no
/// build target is configured and to `<target_dir>/<target_tuple>/release/`
/// when `[build] target` is set in `.cargo/config.toml` or `--target` is
/// passed. Both layouts can coexist on disk — leftover artefacts from a
/// previous toolchain or a one-off non-default build sit alongside the
/// current one. The resolver collects every candidate matching either
/// layout and returns the most recently modified, which is the artefact
/// cargo produced on the latest build. When no candidate exists at all,
/// the canonical top-level path is returned so the downstream freshness
/// or existence check surfaces a clear error.
pub fn release_bin(target_dir: &Path, name: &str) -> PathBuf {
    let canonical = target_dir.join("release").join(name);
    let mut candidates = vec![canonical.clone()];
    if let Ok(entries) = fs::read_dir(target_dir) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir.file_name().is_some_and(|n| n == "release") {
                continue;
            }
            candidates.push(dir.join("release").join(name));
        }
    }
    candidates
        .into_iter()
        .filter_map(|p| {
            fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| (p, t))
        })
        .max_by_key(|(_, t)| *t)
        .map(|(p, _)| p)
        .unwrap_or(canonical)
}

#[derive(Debug)]
pub enum FreshnessError {
    Stale {
        path: PathBuf,
        bin_mtime: i64,
        head_time: i64,
    },
    HashMismatch {
        path: PathBuf,
        bin_hash: String,
        head_hash: String,
    },
    GitFailed(String),
    Io(std::io::Error),
}

impl fmt::Display for FreshnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FreshnessError::Stale {
                path,
                bin_mtime,
                head_time,
            } => write!(
                f,
                "{} was built {} (Unix ts), current HEAD is {} (Unix ts) — \
                 rebuild release binaries before running integ tests \
                 (or set LEVICULUM_SKIP_FRESHNESS_CHECK=1 for local iteration)",
                path.display(),
                bin_mtime,
                head_time
            ),
            FreshnessError::HashMismatch {
                path,
                bin_hash,
                head_hash,
            } => write!(
                f,
                "{} was built from git {}, repo HEAD is {} — rebuild release \
                 binaries before running integ tests (force-rebuild runs \
                 automatically unless LEVICULUM_SKIP_FORCE_REBUILD=1; freshness \
                 guard opt-out is LEVICULUM_SKIP_FRESHNESS_CHECK=1)",
                path.display(),
                bin_hash,
                head_hash
            ),
            FreshnessError::GitFailed(msg) => write!(f, "git HEAD lookup failed: {msg}"),
            FreshnessError::Io(e) => write!(f, "I/O error during freshness check: {e}"),
        }
    }
}

impl std::error::Error for FreshnessError {}

impl From<std::io::Error> for FreshnessError {
    fn from(e: std::io::Error) -> Self {
        FreshnessError::Io(e)
    }
}

/// Compare every binary's mtime against the most recent commit that
/// modified code contributing to the binaries. Any binary strictly older
/// than that commit fails the check.
///
/// Skipped entirely when `LEVICULUM_SKIP_FRESHNESS_CHECK` is set.
///
/// The path-specific variant (introduced after the 2026-04-17 batch
/// burned hardware time on unrelated test-file commits) asks git for the
/// last commit that touched any Rust source or manifest under the
/// production crates. A commit that only modifies files under any
/// `tests/` directory or `~/.claude/` does not invalidate previously-built
/// binaries, because those files are not linked into the integ
/// artefacts. Falls back to plain HEAD if the path-restricted query
/// fails for any reason so the check stays conservative.
pub fn check_binary_freshness(binaries: &[&Path], repo_root: &Path) -> Result<(), FreshnessError> {
    if std::env::var_os("LEVICULUM_SKIP_FRESHNESS_CHECK").is_some() {
        return Ok(());
    }

    let head_time = git_production_source_commit_time(repo_root)
        .or_else(|_| git_head_commit_time(repo_root))?;

    for bin in binaries {
        let mtime = fs::metadata(bin)?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| FreshnessError::GitFailed(format!("mtime before epoch: {e}")))?
            .as_secs() as i64;

        if mtime < head_time {
            return Err(FreshnessError::Stale {
                path: bin.to_path_buf(),
                bin_mtime: mtime,
                head_time,
            });
        }
    }

    Ok(())
}

/// Parse the git hash embedded in a `--version` line by build.rs, which
/// formats it as `<name> <version> (<hash>)`. Returns the contents of the
/// last parenthesised group. `None` if there is no such group.
pub fn parse_embedded_hash(version_output: &str) -> Option<String> {
    let line = version_output.lines().next()?.trim();
    let open = line.rfind('(')?;
    let close = line[open..].find(')')? + open;
    let hash = line[open + 1..close].trim();
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

/// Run `<bin> --version` and return the git hash build.rs embedded in it.
/// Returns "unknown" when the binary predates this mechanism (no
/// parenthesised hash), so an old binary does not hard-fail the guard.
pub fn binary_git_hash(bin: &Path) -> Result<String, FreshnessError> {
    // An un-runnable binary (non-executable, exec failure) or one that cannot
    // produce a parseable --version is "unknown", not a hard error. The mtime
    // check still guards it; only a runnable binary with a real, mismatched
    // embedded hash is flagged.
    let output = match Command::new(bin).arg("--version").output() {
        Ok(output) => output,
        Err(_) => return Ok("unknown".to_string()),
    };
    if !output.status.success() {
        return Ok("unknown".to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_embedded_hash(&stdout).unwrap_or_else(|| "unknown".to_string()))
}

/// Assert every binary was built from the repo's current HEAD by reading
/// the git hash embedded at compile time (via `<bin> --version`) and
/// comparing it to `git rev-parse HEAD`. Unlike the mtime check, this
/// catches a wrong-branch binary whose mtime is newer than the current
/// commit.
///
/// A binary reporting hash "unknown" (built without git, or before this
/// mechanism existed) is not failed — the mtime check still guards it.
/// The repo HEAD likewise resolving to "unknown" skips the comparison.
///
/// Skipped entirely when `LEVICULUM_SKIP_FRESHNESS_CHECK` is set, sharing
/// the opt-out with the mtime check.
pub fn check_binary_git_hash(binaries: &[&Path], repo_root: &Path) -> Result<(), FreshnessError> {
    if std::env::var_os("LEVICULUM_SKIP_FRESHNESS_CHECK").is_some() {
        return Ok(());
    }

    let head_hash = git_head_hash(repo_root)?;
    if head_hash == "unknown" {
        return Ok(());
    }

    for bin in binaries {
        let bin_hash = binary_git_hash(bin)?;
        if bin_hash != "unknown" && bin_hash != head_hash {
            return Err(FreshnessError::HashMismatch {
                path: bin.to_path_buf(),
                bin_hash,
                head_hash,
            });
        }
    }

    Ok(())
}

/// `git rev-parse HEAD` in `repo_root`, or "unknown" if git is absent.
fn git_head_hash(repo_root: &Path) -> Result<String, FreshnessError> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            Ok(if s.is_empty() {
                "unknown".to_string()
            } else {
                s
            })
        }
        _ => Ok("unknown".to_string()),
    }
}

/// Paths that contribute to the integ binaries. Listed explicitly so a
/// commit touching `tests/` or docs or `.claude/` does not invalidate
/// artefacts. `Cargo.lock` is included because dependency-version
/// changes do rebuild.
const PRODUCTION_SOURCE_PATHS: &[&str] = &[
    "leviculum-core/src",
    "leviculum-core/Cargo.toml",
    "leviculum-std/src",
    "leviculum-std/Cargo.toml",
    "leviculum-cli/src",
    "leviculum-cli/Cargo.toml",
    "leviculum-proxy/src",
    "leviculum-proxy/Cargo.toml",
    "leviculum-nrf/src",
    "leviculum-nrf/Cargo.toml",
    "Cargo.toml",
    "Cargo.lock",
];

fn git_head_commit_time(repo_root: &Path) -> Result<i64, FreshnessError> {
    git_commit_time_for_paths(repo_root, &[])
}

fn git_production_source_commit_time(repo_root: &Path) -> Result<i64, FreshnessError> {
    git_commit_time_for_paths(repo_root, PRODUCTION_SOURCE_PATHS)
}

fn git_commit_time_for_paths(repo_root: &Path, paths: &[&str]) -> Result<i64, FreshnessError> {
    let mut args: Vec<&str> = vec!["log", "-1", "--format=%ct", "HEAD"];
    if !paths.is_empty() {
        args.push("--");
        args.extend(paths);
    }
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|e| FreshnessError::GitFailed(format!("spawn git: {e}")))?;

    if !output.status.success() {
        return Err(FreshnessError::GitFailed(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }

    let ts_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ts_str.is_empty() {
        return Err(FreshnessError::GitFailed(
            "no matching commit for production source paths".to_string(),
        ));
    }
    ts_str
        .parse::<i64>()
        .map_err(|e| FreshnessError::GitFailed(format!("parse '{ts_str}': {e}")))
}

/// Fingerprint used in diagnostic output. Not part of the public API
/// contract; tests pin the format lightly.
pub fn describe_binary(path: &Path) -> String {
    match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => format!("{} (mtime={})", path.display(), d.as_secs()),
            Err(_) => path.display().to_string(),
        },
        Err(_) => format!("{} (missing)", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restore `key` to exactly its pre-test state: prior-present → set
    /// back, prior-absent → remove. The CI tiers run this suite in one
    /// process with `CARGO_TARGET_DIR` set for the whole process
    /// (scripts/run-tier2.sh); a test that leaves the var deleted
    /// redirects every later test in the same process to the repo-local
    /// target dir — the 2026-06-11 tier2 RED (basic_probe_lifecycle
    /// StaleBinary).
    fn restore_env(key: &str, prior: Option<std::ffi::OsString>) {
        // SAFETY: the suite runs single-threaded (--test-threads=1 in all
        // CI tiers and the documented local invocation), so no other
        // thread observes the mutation.
        match &prior {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
        assert_eq!(
            std::env::var_os(key),
            prior,
            "{key} must be back to its pre-test state"
        );
    }

    #[test]
    fn target_dir_respects_env_var() {
        let repo = Path::new("/tmp/fake-repo");
        let prior = std::env::var_os("CARGO_TARGET_DIR");
        // SAFETY: single-threaded suite (--test-threads=1 in all CI
        // tiers); this test is itself a modifier of the var, so the prior
        // value is restored exactly via restore_env below.
        unsafe { std::env::set_var("CARGO_TARGET_DIR", "/tmp/other-target") };
        assert_eq!(target_dir(repo), PathBuf::from("/tmp/other-target"));
        unsafe { std::env::remove_var("CARGO_TARGET_DIR") };
        assert_eq!(target_dir(repo), PathBuf::from("/tmp/fake-repo/target"));
        restore_env("CARGO_TARGET_DIR", prior);
    }

    #[test]
    fn release_bin_joins_release() {
        let td = Path::new("/tmp/build");
        assert_eq!(
            release_bin(td, "lnsd"),
            PathBuf::from("/tmp/build/release/lnsd")
        );
    }

    #[test]
    fn release_bin_falls_back_to_target_subdir_when_top_level_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let musl = tmp.path().join("x86_64-unknown-linux-musl").join("release");
        std::fs::create_dir_all(&musl).expect("create_dir_all");
        let bin = musl.join("lnsd");
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").expect("write fake binary");
        assert_eq!(release_bin(tmp.path(), "lnsd"), bin);
    }

    #[test]
    fn release_bin_prefers_more_recently_modified_candidate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let top = tmp.path().join("release");
        let musl = tmp.path().join("x86_64-unknown-linux-musl").join("release");
        std::fs::create_dir_all(&top).expect("create_dir_all top");
        std::fs::create_dir_all(&musl).expect("create_dir_all musl");
        let top_bin = top.join("lnsd");
        let musl_bin = musl.join("lnsd");
        std::fs::write(&top_bin, b"top older").expect("write top");
        // Filesystem mtime resolution is 1 s on some filesystems; sleep
        // strictly more than that to make the relative ordering reliable.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&musl_bin, b"musl newer").expect("write musl");
        assert_eq!(release_bin(tmp.path(), "lnsd"), musl_bin);
    }

    #[test]
    fn release_bin_picks_top_level_when_top_level_is_newer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let top = tmp.path().join("release");
        let musl = tmp.path().join("x86_64-unknown-linux-musl").join("release");
        std::fs::create_dir_all(&top).expect("create_dir_all top");
        std::fs::create_dir_all(&musl).expect("create_dir_all musl");
        let top_bin = top.join("lnsd");
        let musl_bin = musl.join("lnsd");
        std::fs::write(&musl_bin, b"musl older").expect("write musl");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&top_bin, b"top newer").expect("write top");
        assert_eq!(release_bin(tmp.path(), "lnsd"), top_bin);
    }

    #[test]
    fn parse_embedded_hash_extracts_paren_group() {
        assert_eq!(
            parse_embedded_hash("lnsd 0.7.0 (deadbeef)\n"),
            Some("deadbeef".to_string())
        );
        // Nightly version with its own suffix plus the trailing hash:
        // the LAST parenthesised group wins.
        assert_eq!(
            parse_embedded_hash("lnstest 0.7.0-nightly.20260419-5a5df20 (cafef00d)"),
            Some("cafef00d".to_string())
        );
        assert_eq!(
            parse_embedded_hash("lncp 0.7.0 (unknown)"),
            Some("unknown".to_string())
        );
    }

    #[test]
    fn parse_embedded_hash_none_without_group() {
        assert_eq!(parse_embedded_hash("lnsd 0.7.0"), None);
        assert_eq!(parse_embedded_hash(""), None);
        assert_eq!(parse_embedded_hash("lnsd 0.7.0 ()"), None);
    }

    /// Write an executable shell script that ignores its args and prints
    /// `line` to stdout, so it stands in for a real binary's `--version`.
    #[cfg(unix)]
    fn write_fake_bin(dir: &Path, name: &str, line: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{line}'\n")).expect("write fake bin");
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    /// Initialise a throwaway git repo with one commit and return its HEAD.
    fn init_repo_with_commit(dir: &Path) -> String {
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .expect("git")
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f"), b"x").expect("write");
        run(&["add", "f"]);
        run(&["commit", "-q", "-m", "c"]);
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .expect("rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn hash_guard_matches_head() {
        let prior = std::env::var_os("LEVICULUM_SKIP_FRESHNESS_CHECK");
        // SAFETY: single-threaded suite (--test-threads=1 in all CI tiers).
        unsafe { std::env::remove_var("LEVICULUM_SKIP_FRESHNESS_CHECK") };
        let tmp = tempfile::tempdir().expect("tempdir");
        let head = init_repo_with_commit(tmp.path());
        let bin = write_fake_bin(tmp.path(), "lnsd", &format!("lnsd 0.7.0 ({head})"));
        let result = check_binary_git_hash(&[bin.as_path()], tmp.path());
        restore_env("LEVICULUM_SKIP_FRESHNESS_CHECK", prior);
        assert!(result.is_ok(), "matching hash must pass: {result:?}");
    }

    #[cfg(unix)]
    #[test]
    fn hash_guard_rejects_wrong_branch_binary() {
        let prior = std::env::var_os("LEVICULUM_SKIP_FRESHNESS_CHECK");
        // SAFETY: single-threaded suite (--test-threads=1 in all CI tiers).
        unsafe { std::env::remove_var("LEVICULUM_SKIP_FRESHNESS_CHECK") };
        let tmp = tempfile::tempdir().expect("tempdir");
        let _head = init_repo_with_commit(tmp.path());
        // A binary built from a different commit than the repo HEAD.
        let bin = write_fake_bin(
            tmp.path(),
            "lnsd",
            "lnsd 0.7.0 (0000000000000000000000000000000000000000)",
        );
        let result = check_binary_git_hash(&[bin.as_path()], tmp.path());
        restore_env("LEVICULUM_SKIP_FRESHNESS_CHECK", prior);
        match result {
            Err(FreshnessError::HashMismatch { bin_hash, .. }) => {
                assert_eq!(bin_hash, "0000000000000000000000000000000000000000");
            }
            other => panic!("expected HashMismatch, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn hash_guard_allows_unknown_binary_hash() {
        let prior = std::env::var_os("LEVICULUM_SKIP_FRESHNESS_CHECK");
        // SAFETY: single-threaded suite (--test-threads=1 in all CI tiers).
        unsafe { std::env::remove_var("LEVICULUM_SKIP_FRESHNESS_CHECK") };
        let tmp = tempfile::tempdir().expect("tempdir");
        let _head = init_repo_with_commit(tmp.path());
        // Binary built without git reports "unknown" and must not hard-fail
        // (the mtime check still guards it).
        let bin = write_fake_bin(tmp.path(), "lnsd", "lnsd 0.7.0 (unknown)");
        let result = check_binary_git_hash(&[bin.as_path()], tmp.path());
        restore_env("LEVICULUM_SKIP_FRESHNESS_CHECK", prior);
        assert!(result.is_ok(), "unknown bin hash must pass: {result:?}");
    }

    #[test]
    fn freshness_skipped_when_env_set() {
        let prior = std::env::var_os("LEVICULUM_SKIP_FRESHNESS_CHECK");
        // SAFETY: single-threaded suite (--test-threads=1 in all CI
        // tiers); the prior value is restored exactly via restore_env
        // below — deleting it would clobber a developer's intentional
        // skip flag for subsequent tests in the same process.
        unsafe { std::env::set_var("LEVICULUM_SKIP_FRESHNESS_CHECK", "1") };
        // Passing a nonexistent path should still succeed under the skip
        // env var, because the function returns before touching the fs.
        let result = check_binary_freshness(&[Path::new("/nonexistent/path")], Path::new("/tmp"));
        restore_env("LEVICULUM_SKIP_FRESHNESS_CHECK", prior);
        assert!(result.is_ok());
    }
}
