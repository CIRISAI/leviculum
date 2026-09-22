//! `LEVICULUM_EVENT_LOG` survives a `copytruncate` rotation and nothing
//! else.
//!
//! A public backbone node's event log reaches tens of gigabytes (miauhaus:
//! 60 GB), and the host we are about to move `leviculum.network` onto has
//! about 29 GB free. So the log has to be rotated, and rotation has exactly
//! two shapes: `copytruncate` (copy the bytes aside, truncate the original
//! in place, same inode) or the default rename-and-create (move the file
//! aside, create a new one, new inode, and signal the writer to reopen).
//!
//! Which one applies is not a preference: the writer opens the file ONCE
//! per process and caches the handle in a `OnceLock`
//! (`leviculum-std/src/event_log.rs:1066-1085`), and there is no reopen path
//! and no signal handler anywhere that could give it a new one. Under
//! rename-and-create the daemon therefore keeps writing into the renamed
//! inode for ever: the new file stays empty, the old one keeps growing, and
//! the disk is never freed — the precise failure the rotation was installed
//! to prevent, arriving silently.
//!
//! Both halves are asserted here. The second is the positive control: it
//! injects the wrong rotation and shows the damage, so the first test
//! cannot pass for the wrong reason.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Locate the compiled `event-log-helper`, building it if missing.
/// Deliberately the same resolution `event_log_multiprocess.rs` uses.
fn helper_bin() -> PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let triple = std::env::var("CARGO_BUILD_TARGET")
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string());

    let candidates = [
        target_dir
            .join(&triple)
            .join("debug")
            .join("event-log-helper"),
        target_dir.join("debug").join("event-log-helper"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    let status = Command::new(env!("CARGO"))
        .args(["build", "--bin", "event-log-helper", "-p", "leviculum-std"])
        .status()
        .expect("cargo build for event-log-helper");
    assert!(status.success(), "cargo build failed");
    candidates
        .iter()
        .find(|c| c.exists())
        .unwrap_or_else(|| panic!("event-log-helper not found after build"))
        .clone()
}

/// Start a helper that emits its first event, then blocks on `gate`.
///
/// Supervised, not bare: this child deliberately waits on a file the test
/// creates, so a test binary that dies between the spawn and the gate
/// leaves it blocking. The kernel link ends it with its parent instead.
fn spawn_gated(log: &Path, gate: &Path) -> Child {
    let mut cmd = Command::new(helper_bin());
    cmd.arg("0")
        .arg("10")
        .arg(gate)
        .env("LEVICULUM_EVENT_LOG", log)
        .env("LEVICULUM_EVENT_NODE", "rotation");
    leviculum_std::process::spawn_supervised(cmd).expect("helper starts")
}

fn count_ticks(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|text| text.matches("HELPER_TICK").count())
        .unwrap_or(0)
}

/// Block until the log holds at least one event, so the rotation below
/// lands between two events of the same process rather than before all of
/// them. Bounded; a timeout is a failure, not a retry.
fn wait_for_first_event(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while count_ticks(path) == 0 {
        assert!(
            Instant::now() < deadline,
            "the helper never wrote its first event to {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn appending_continues_through_a_copytruncate_rotation() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("events.log");
    let rotated = dir.path().join("events.log.1");
    let gate = dir.path().join("gate");

    let mut helper = spawn_gated(&log, &gate);
    wait_for_first_event(&log);

    // What logrotate's `copytruncate` does, in the order it does it: copy
    // the bytes aside, then truncate the original in place. Same inode, so
    // the writer's cached handle still points at the file an operator is
    // looking at.
    std::fs::copy(&log, &rotated).expect("copy aside");
    std::fs::File::create(&log).expect("truncate in place");

    let before_rotation = count_ticks(&rotated);
    assert!(
        before_rotation >= 1,
        "the rotated copy must hold the events that were already written"
    );

    std::fs::write(&gate, b"go").expect("open the gate");
    assert!(helper.wait().expect("helper exits").success());

    let after_rotation = count_ticks(&log);
    assert_eq!(
        before_rotation + after_rotation,
        3,
        "every event must land somewhere: {before_rotation} before the rotation, \
         {after_rotation} after, of 3 emitted"
    );
    assert!(
        after_rotation >= 1,
        "the live file must keep growing after the truncate; it held {after_rotation} event(s), \
         which means the writer stopped appending where an operator would look"
    );
}

/// The positive control: rename-and-create, the logrotate default. The
/// writer has no reopen path, so it keeps filling the renamed inode and the
/// file the operator (and any disk-usage alarm) watches stays empty. If
/// this test ever goes green as "the new file also grew", the writer has
/// gained a reopen and the `copytruncate` directive can be dropped.
#[test]
fn a_rename_rotation_leaves_the_new_file_empty() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("events.log");
    let renamed = dir.path().join("events.log.1");
    let gate = dir.path().join("gate");

    let mut helper = spawn_gated(&log, &gate);
    wait_for_first_event(&log);

    std::fs::rename(&log, &renamed).expect("rename aside");

    std::fs::write(&gate, b"go").expect("open the gate");
    assert!(helper.wait().expect("helper exits").success());

    assert_eq!(
        count_ticks(&log),
        0,
        "without a reopen the writer cannot find the new file; if this is non-zero, \
         event_log.rs has gained a reopen path and packaging/logrotate/leviculum \
         should say so"
    );
    assert_eq!(
        count_ticks(&renamed),
        3,
        "all three events went into the renamed inode, which is the disk that never frees"
    );
}
