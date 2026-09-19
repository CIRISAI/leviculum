use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    emit_build_unix_secs();
}

// Embed a unix-seconds build timestamp for the calendar's birth anchor and
// the sanity window's lower bound
// (docs/src/concepts/time-and-clocks.md, "The sanity window", "Arm 5: The
// build floor"). The only property the model needs from this value is that
// real time is always AFTER it; precision buys nothing, so every source
// below is chosen to be in the past by construction.
//
// In order:
//
// 1. `LEVICULUM_BUILD_UNIX_SECS` — an explicit override, for a port or a
//    packager that has its own notion of build time and for the negative
//    tests of downstream crates.
// 2. `SOURCE_DATE_EPOCH` — the reproducible-builds standard. A distro
//    builder sets it, and honouring it keeps our binaries bit-identical
//    across rebuilds instead of carrying the wall clock of the build host.
// 3. The committer timestamp of `HEAD`. Reproducible for the same commit,
//    always in the past, and it advances with development on its own.
// 4. The wall clock of the build host, if there is no git to ask (a release
//    tarball).
//
// Not `rerun-if-changed` on `HEAD`: leviculum-core is the dependency of
// every crate in the workspace, so re-embedding the stamp on every commit
// would rebuild the whole tree on every commit. A stamp that stays at the
// commit the target directory was first built from is STALE, never wrong:
// a floor below real time only widens the sanity window, and the clean
// builds that ship (CI, release, a fresh clone) carry a current one.
fn emit_build_unix_secs() {
    let secs = env_secs("LEVICULUM_BUILD_UNIX_SECS")
        .or_else(|| env_secs("SOURCE_DATE_EPOCH"))
        .or_else(git_head_unix_secs)
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
        .unwrap_or(0);

    println!("cargo:rustc-env=LEVICULUM_BUILD_UNIX_SECS={secs}");

    // Emitting ANY rerun-if-changed replaces cargo's default "rerun on any
    // package file change", which is exactly the churn this avoids: without
    // it every edit under `src/` would re-stamp and rebuild the crate.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LEVICULUM_BUILD_UNIX_SECS");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}

fn env_secs(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

fn git_head_unix_secs() -> Option<u64> {
    Command::new("git")
        .args(["log", "-1", "--format=%ct", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
}
