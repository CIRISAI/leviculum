//! An `lnsd` built without one of leviculum-std's interface families refuses
//! a config section of that family and names the feature (Codeberg #463).
//!
//! Observable from outside only: the catalogue's refusal is a unit test in
//! leviculum-std, but whether the daemon turns it into a failed start rather
//! than a node that comes up without the radio its operator configured is
//! the binary's behaviour. Each test exists only in the build that lacks the
//! feature, so `just lnsd-minimal` is what runs them.
#![cfg(any(
    not(feature = "serial"),
    not(feature = "i2p"),
    all(not(feature = "ble"), target_os = "linux")
))]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

/// Start `lnsd` on a config holding one `[[Radio]]` section of `kind` plus
/// `extra` keys, and return its exit status and stderr. Killed after 20 s:
/// a daemon still running then has accepted the section, which is the
/// failure under test.
fn lnsd_with_section(kind: &str, extra: &str) -> (Option<i32>, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut f = std::fs::File::create(dir.path().join("config")).expect("write config");
    writeln!(
        f,
        "[reticulum]\n  enable_transport = False\n  share_instance = No\n[logging]\n  loglevel = 3\n\
         [interfaces]\n  [[Radio]]\n    type = {kind}\n    enabled = yes\n{extra}"
    )
    .expect("write config body");
    drop(f);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("--config")
        .arg(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = spawn_supervised(cmd).expect("spawn lnsd");
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll lnsd") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
    }
    (status.and_then(|s| s.code()), stderr)
}

#[cfg(not(feature = "serial"))]
#[test]
fn a_serial_section_fails_the_start_and_names_the_feature() {
    let (code, stderr) =
        lnsd_with_section("SerialInterface", "    port = /dev/null-no-such-port\n");
    assert_eq!(
        code,
        Some(1),
        "lnsd must refuse to start, not run without the radio:\n{stderr}"
    );
    assert!(
        stderr.contains("'Radio'")
            && stderr.contains("SerialInterface")
            && stderr.contains("`serial`"),
        "the refusal names the section, the kind and the feature:\n{stderr}"
    );
}

#[cfg(all(not(feature = "ble"), target_os = "linux"))]
#[test]
fn a_ble_section_fails_the_start_and_names_the_feature() {
    let (code, stderr) = lnsd_with_section("BLEInterface", "");
    assert_eq!(
        code,
        Some(1),
        "lnsd must refuse to start, not run without BLE:\n{stderr}"
    );
    assert!(
        stderr.contains("'Radio'") && stderr.contains("BLEInterface") && stderr.contains("`ble`"),
        "the refusal names the section, the kind and the feature:\n{stderr}"
    );
}

#[cfg(not(feature = "i2p"))]
#[test]
fn an_i2p_section_fails_the_start_and_names_the_feature() {
    let (code, stderr) = lnsd_with_section("I2PInterface", "    connectable = yes\n");
    assert_eq!(
        code,
        Some(1),
        "lnsd must refuse to start, not run without I2P:\n{stderr}"
    );
    assert!(
        stderr.contains("'Radio'") && stderr.contains("I2PInterface") && stderr.contains("`i2p`"),
        "the refusal names the section, the kind and the feature:\n{stderr}"
    );
}
