use std::path::{Path, PathBuf};

use reticulum_integ::paths;

/// CLI entrypoint for the integ framework.
///
/// `check-freshness` is the nightly's pre-flight gate: it runs the EXACT
/// `paths::check_binary_freshness` the per-test `TestRunner::new` runs, so
/// the build step and the test-time freshness assertion cannot drift apart
/// (one source of truth, one binary). run-tier3-hw.sh calls it once after
/// the build and aborts the whole run on a single clear failure instead of
/// letting N scenarios die individually in setup (2026-06-13 nightly).
fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("check-freshness") => check_freshness(&args[2..]),
        _ => {
            println!("reticulum-integ: integration test framework");
            println!("Usage:");
            println!("  cargo test -p reticulum-integ");
            println!("  reticulum-integ check-freshness [<repo_root> <target_dir>]");
        }
    }
}

/// Assert the mounted integ binaries are newer than the last
/// production-source commit. Exit 0 = fresh, 2 = stale, 1 = error.
///
/// With no args, resolves repo_root + target_dir exactly as
/// `TestRunner::new` does (so it checks the same artefacts the tests
/// mount). The optional `<repo_root> <target_dir>` positionals exist only
/// for the regression test, which stages a deliberately-stale binary.
fn check_freshness(args: &[String]) -> ! {
    let (repo_root, target_dir): (PathBuf, PathBuf) = if args.len() == 2 {
        (PathBuf::from(&args[0]), PathBuf::from(&args[1]))
    } else {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest
            .parent()
            .expect("CARGO_MANIFEST_DIR has no parent")
            .to_path_buf();
        let target_dir = paths::target_dir(&repo_root);
        (repo_root, target_dir)
    };

    // The four production binaries the runner mounts. lora-proxy is only
    // mounted for proxy scenarios, but the build always produces all four
    // and the nightly always has proxy scenarios, so checking the full set
    // is the safe superset.
    let names = ["lnsd", "lns", "lncp", "lora-proxy"];
    let bins: Vec<PathBuf> = names
        .iter()
        .map(|n| paths::release_bin(&target_dir, n))
        .collect();
    let refs: Vec<&Path> = bins.iter().map(PathBuf::as_path).collect();

    // Commit-hash guard. lnsd/lns/lncp embed their build hash via build.rs;
    // lora-proxy has no such seam, so only the cli bins are hash-checked.
    let hash_refs: Vec<&Path> = bins
        .iter()
        .filter(|p| {
            p.file_name()
                .is_none_or(|n| n.to_string_lossy() != "lora-proxy")
        })
        .map(PathBuf::as_path)
        .collect();
    if let Err(e) = paths::check_binary_git_hash(&hash_refs, &repo_root) {
        match e {
            paths::FreshnessError::HashMismatch { .. } => {
                eprintln!("[preflight] WRONG-BRANCH integ binary:");
                eprintln!("[preflight]   {e}");
                std::process::exit(2);
            }
            other => {
                eprintln!("[preflight] hash guard error: {other}");
                std::process::exit(1);
            }
        }
    }

    match paths::check_binary_freshness(&refs, &repo_root) {
        Ok(()) => {
            println!(
                "[preflight] integ binaries fresh ({} checked, {} hash-verified)",
                refs.len(),
                hash_refs.len()
            );
            std::process::exit(0);
        }
        Err(paths::FreshnessError::Stale { .. }) => {
            eprintln!("[preflight] STALE integ binaries (build did not relink after repo-sync):");
            for bin in &refs {
                if let Err(e) = paths::check_binary_freshness(&[bin], &repo_root) {
                    eprintln!("[preflight]   {e}");
                }
            }
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("[preflight] freshness check error: {e}");
            std::process::exit(1);
        }
    }
}
