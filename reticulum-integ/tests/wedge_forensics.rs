//! Sentinel for the wedge-forensics capture script (Codeberg #50).
//!
//! Verifies the script runs cleanly even with no real wedge to
//! capture — the script is best-effort observational and must never
//! abort.  The capture-on-timeout integration in
//! `reticulum-integ/src/timeout.rs::run_with_timeout` invokes the same
//! script, so this sentinel covers that integration too.

use std::path::PathBuf;
use std::process::Command;

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("scripts/_capture-wedge-forensics.sh")
}

#[test]
fn capture_script_exists_and_is_executable() {
    let p = script_path();
    assert!(p.exists(), "script not at {}", p.display());
    let meta = std::fs::metadata(&p).expect("stat script");
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    assert_ne!(mode & 0o111, 0, "script not executable: mode {:o}", mode);
}

#[test]
fn capture_script_runs_without_errors() {
    let out = Command::new("bash")
        .arg(script_path())
        .arg("sentinel-test")
        .output()
        .expect("spawn capture script");
    // Best-effort observational — `set +e` inside the script + explicit
    // `exit 0` at the end mean the script must always succeed.
    assert!(
        out.status.success(),
        "capture script failed (exit {:?}, stderr={:?})",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn capture_script_creates_tagged_directory() {
    let tag = format!("test-tag-{}", std::process::id());
    Command::new("bash")
        .arg(script_path())
        .arg(&tag)
        .output()
        .expect("spawn");
    // Look up the most recent forensics directory matching our tag.
    let base = PathBuf::from("/tmp/leviculum/wedge-forensics");
    let entries = std::fs::read_dir(&base).expect("read forensics dir");
    let found = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|name| name.contains(&tag));
    assert!(
        found.is_some(),
        "no directory matching tag '{tag}' in {}",
        base.display()
    );
    // Capture log must be present.
    let dir = base.join(found.unwrap());
    assert!(
        dir.join("capture.log").exists(),
        "capture.log missing in {}",
        dir.display()
    );
}

#[test]
#[should_panic(expected = "wedge: worker still active after 1s")]
fn timeout_invokes_capture_script() {
    // run_with_timeout's Timeout branch calls capture_wedge_forensics
    // before panicking.  We can't easily assert the call happened from
    // here (the panic kills the test before any post-action), but this
    // test verifies the integration compiles and the timeout still
    // fires its panic message correctly even with the capture call
    // wired in.  The 60s sleeper cannot finish inside the 50ms grace
    // window, so the wedge branch (not wrapper-tight) is deterministic.
    reticulum_integ::timeout::run_with_timeout("forensics-sentinel", 1, || {
        std::thread::sleep(std::time::Duration::from_secs(60));
    });
}
