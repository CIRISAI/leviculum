//! Root-1 regression (2026-06-13 nightly): 12 scenarios aborted in setup
//! with `stale binary: lora-proxy was built <t>, current HEAD is <t+4.5h>`.
//!
//! After a repo-sync pulls newer commits, cargo can decide the binaries
//! are "up to date" and skip the relink, leaving their mtime older than
//! the new HEAD's production-source commit time — which
//! `check_binary_freshness` then rejects, once per binary-mounting test.
//!
//! `run-tier3-hw.sh` now runs the SAME check ONCE as a preflight
//! (`reticulum-integ check-freshness`) and aborts the whole run on a
//! single clear failure. This test drives that exact preflight entrypoint
//! against a staged stale binary (exit 2 = stale) and a fresh one
//! (exit 0), so the one-loud-failure contract cannot silently regress.
//! Hardware-free.

use std::path::Path;
use std::process::Command;

const PREFLIGHT_BIN: &str = env!("CARGO_BIN_EXE_reticulum-integ");

/// Build a temp git repo with one commit under a production-source path,
/// plus a target dir holding four release binaries. Returns
/// (tempdir, repo_root, target_dir, head_commit_unixtime).
fn stage(
    test_tag: &str,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    i64,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let prod = repo.join("reticulum-core").join("src");
    std::fs::create_dir_all(&prod).expect("mkdir prod");
    std::fs::write(prod.join("lib.rs"), format!("// {test_tag}\n")).expect("write src");

    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "production source"]);

    let head_time: i64 = String::from_utf8(
        Command::new("git")
            .args(["log", "-1", "--format=%ct", "HEAD"])
            .current_dir(&repo)
            .output()
            .expect("git log")
            .stdout,
    )
    .unwrap()
    .trim()
    .parse()
    .expect("parse %ct");

    let target = tmp.path().join("target");
    let release = target.join("release");
    std::fs::create_dir_all(&release).expect("mkdir release");
    for name in ["lnsd", "lnstest", "lncp", "lora-proxy"] {
        std::fs::write(release.join(name), b"#!/bin/sh\nexit 0\n").expect("write bin");
    }

    (tmp, repo, target, head_time)
}

/// Set a file's mtime to a unix timestamp via `touch -d @<ts>`.
fn set_mtime(path: &Path, unixtime: i64) {
    let ok = Command::new("touch")
        .args(["-d", &format!("@{unixtime}"), &path.display().to_string()])
        .status()
        .expect("touch")
        .success();
    assert!(ok, "touch failed for {}", path.display());
}

fn run_preflight(repo: &Path, target: &Path) -> i32 {
    Command::new(PREFLIGHT_BIN)
        .arg("check-freshness")
        .arg(repo)
        .arg(target)
        .status()
        .expect("run preflight")
        .code()
        .expect("preflight exit code")
}

#[test]
fn preflight_flags_binary_older_than_production_commit() {
    let (_tmp, repo, target, head_time) = stage("stale");
    let release = target.join("release");
    // Every binary predates the production commit by 100 s — the exact
    // shape of the nightly leftover (built before the synced HEAD).
    for name in ["lnsd", "lnstest", "lncp", "lora-proxy"] {
        set_mtime(&release.join(name), head_time - 100);
    }
    assert_eq!(
        run_preflight(&repo, &target),
        2,
        "preflight must exit 2 (stale) when binaries predate the production commit"
    );
}

#[test]
fn preflight_passes_when_binaries_postdate_production_commit() {
    let (_tmp, repo, target, head_time) = stage("fresh");
    let release = target.join("release");
    // Simulate a successful relink: binaries stamped after the commit.
    for name in ["lnsd", "lnstest", "lncp", "lora-proxy"] {
        set_mtime(&release.join(name), head_time + 100);
    }
    assert_eq!(
        run_preflight(&repo, &target),
        0,
        "preflight must exit 0 (fresh) when binaries postdate the production commit"
    );
}
