//! The device walk's parsers (1175) and the power proxy's (1177):
//! `scripts/device-walk.py` reads the screen through a UI dump and the
//! pixels of a screencap and writes one `WALK` line a step;
//! `scripts/power-proxy.py` reads `ss`, `/proc` and dumpsys and writes one
//! `POWER` line a phase. Both need a device and run on the reviewer's host
//! (docs/src/concepts/loquax-device-walk.md,
//! docs/src/concepts/loquax-phone-power.md); what they read and write is
//! pinned here, by their unit tests under `scripts/tests/`, so `cargo test
//! -p loquax-android` fails when a parser breaks.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn the_walks_parsers_pass_their_tests() {
    let tests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/tests");
    let output = Command::new("python3")
        .args(["-I", "-m", "unittest", "discover", "-v", "-s"])
        .arg(&tests)
        .output()
        .expect("python3 runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the walk's parser tests failed:\n{stderr}"
    );
    // Discovery that finds nothing also exits 0 on older Pythons.
    assert!(!stderr.contains("Ran 0 tests"), "no tests ran:\n{stderr}");
}
