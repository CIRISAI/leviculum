//! mvr: a config file with a syntax error must not produce a running daemon.
//!
//! The file here is the one from the `lblogd` restart-limit measurement — an
//! unclosed `[reticulum` and a line of noise. Before the fix, lnsd read it as
//! an empty config, generated an identity, logged `Node started with 0
//! interface(s)` and `Reticulum daemon running`, and stayed up. Under systemd
//! that is `active` forever, with no interfaces, no traffic, and nothing for
//! an operator to find.
//!
//! **The reference decides this, not taste.** Same bytes, same location,
//! `reference/Reticulum` 1.3.5, measured 2026-09-22:
//!
//! ```text
//! $ PYTHONPATH=reference/Reticulum python3 -u RNS/Utilities/rnsd.py --config /tmp/refcfg-a
//! [2026-09-22 14:32:11] [Error]    Could not parse the configuration at /tmp/refcfg-a/config
//! [2026-09-22 14:32:11] [Error]    Check your configuration file for errors!
//! $ echo $?
//! 255
//! ```
//!
//! ConfigObj raises (`Invalid line ('[reticulum') (matched as neither section
//! nor keyword) at line 1.`), and `RNS/Reticulum.py:330-333` turns that into
//! two error lines and `RNS.panic()` — `os._exit(255)`, before a single
//! interface is brought up. So the tolerance was ours alone, and this is what
//! it costs.
//!
//! Why a spawned daemon and not a `Config::load` unit test: the unit test
//! cannot see the failure mode. The defect was never "the parser returns the
//! wrong struct" — it was "the process comes up anyway", and only a process
//! can be asked whether it did.
//!
//! **Acceptance**: red before the fix (lnsd is still alive at the deadline,
//! having logged `Reticulum daemon running`), green after (it exits non-zero
//! within the deadline and names the offending line).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

/// How long the daemon is given to refuse. It refuses before it logs its own
/// version line; this is slack for a loaded CI host, not a measurement.
const REFUSAL_DEADLINE: Duration = Duration::from_secs(3);

/// The file as the `lblogd` pass wrote it: an unclosed section header, then a
/// line that is not a `key = value` pair in any format.
const BROKEN_CONFIG: &str = "[reticulum\nthis is not toml or ini at all = = =\n";

/// Resolve the release binary the integ runner and the mvr tier share.
///
/// Same four lines of path arithmetic as the other mvr files that drive a real
/// daemon, and local for the same reason (see `media_silence_restore_signal`).
fn release_bin(name: &str) -> PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let triple = std::env::var("CARGO_BUILD_TARGET")
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string());
    let triple_release = target_dir.join(&triple).join("release").join(name);
    if triple_release.exists() {
        triple_release
    } else {
        target_dir.join("release").join(name)
    }
}

#[test]
fn lnsd_refuses_an_unparsable_config_instead_of_running_empty() {
    let lnsd = release_bin("lnsd");
    assert!(
        lnsd.exists(),
        "{} not found - run `cargo build --release --bin lnsd` first (or `just build-integ-bins`)",
        lnsd.display()
    );

    let dir = tempfile::Builder::new()
        .prefix("mvr_unparsable_config_")
        .tempdir()
        .expect("temp config dir");
    fs::write(dir.path().join("config"), BROKEN_CONFIG).expect("write the broken config");

    let mut cmd = Command::new(&lnsd);
    cmd.arg("--config")
        .arg(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_supervised(cmd).expect("spawn lnsd");

    let deadline = Instant::now() + REFUSAL_DEADLINE;
    let status = loop {
        match child.try_wait().expect("poll lnsd") {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => break None,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    // A daemon that is still up holds the write end of both pipes open, so
    // reading them to EOF first would hang the test instead of failing it —
    // which is exactly what the red run did. Kill first, read after: the
    // buffered lines survive the child and are the evidence of what it said
    // while coming up on a file it could not read.
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut output = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut output);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut output);
    }

    let Some(status) = status else {
        panic!(
            "lnsd was still running {REFUSAL_DEADLINE:?} after being handed a config file that \
             rnsd exits 255 on. Its output:\n{output}"
        );
    };

    assert!(
        !status.success(),
        "lnsd exited 0 on an unparsable config; rnsd exits 255. Output:\n{output}"
    );
    assert!(
        !output.contains("Reticulum daemon running"),
        "lnsd announced itself running before giving up on the config. Output:\n{output}"
    );
    assert!(
        output.contains("Invalid line 1"),
        "the refusal must name the line that broke, the way ConfigObj's does. Output:\n{output}"
    );
    assert!(
        output.contains("config"),
        "the refusal must name the file it could not read. Output:\n{output}"
    );
}
