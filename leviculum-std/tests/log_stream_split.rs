//! A CLI's diagnostics go to stderr, its data to stdout.
//!
//! `event_log::install_global_subscriber` used `tracing_subscriber`'s
//! default writer, which is stdout — the channel `lnmsg` prints message
//! bodies on, `lncp` its progress, `lnprobe` its reply lines. The moment
//! anything warned, the log line was spliced into that payload. It
//! showed up as `lnmsg/tests/python_interop.rs` going red under load:
//! `CORE_PROCESSOR_OVER_BUDGET` (`driver::processor::report_budget`)
//! landed between a caller's pipe and an assertion that a successful
//! send says nothing on stdout.
//!
//! This test pins the split from outside the process, which is the only
//! place the two streams are distinguishable. `log-stream-helper`
//! installs the same subscriber the production binaries install and
//! drives the genuine over-budget path; here we check which stream each
//! kind of output came out on.
//!
//! Both halves are asserted on purpose. A test that only checked stdout
//! for emptiness would pass just as well against a process that never
//! warned at all, which is the failure mode that let this ship.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate the compiled `log-stream-helper` binary, honouring
/// `CARGO_TARGET_DIR` and the `--target=x86_64-unknown-linux-musl`
/// workspace default in `.cargo/config.toml`. Builds it if missing.
/// Same shape as `event_log_multiprocess.rs::helper_bin`.
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
            .join("log-stream-helper"),
        target_dir.join("debug").join("log-stream-helper"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }

    let status = Command::new(env!("CARGO"))
        .args(["build", "--bin", "log-stream-helper", "-p", "leviculum-std"])
        .status()
        .expect("cargo build for log-stream-helper");
    assert!(status.success(), "cargo build failed");

    candidates
        .iter()
        .find(|c| c.exists())
        .unwrap_or_else(|| panic!("log-stream-helper not found after build; tried: {candidates:?}"))
        .clone()
}

#[test]
fn over_budget_warning_goes_to_stderr_and_leaves_stdout_to_the_payload() {
    let bin = helper_bin();

    // 2500 ms: the driver's timer branch is at most ~1 s apart, so this
    // covers two ticks even if the first is swallowed by startup.
    let output = Command::new(&bin)
        .arg("2500")
        .output()
        .expect("run log-stream-helper");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let context = format!(
        "exit {:?}\nstdout: {stdout:?}\nstderr: {stderr}",
        output.status
    );

    assert!(output.status.success(), "helper exited non-zero: {context}");

    // Positive control first: without the warning this test proves nothing.
    assert!(
        stderr.contains("CORE_PROCESSOR_OVER_BUDGET"),
        "the over-budget warning must reach stderr: {context}"
    );

    // And stdout carries the payload, byte for byte, with nothing spliced in.
    assert_eq!(
        stdout, "PAYLOAD\n",
        "stdout is the data channel and must carry only the payload: {context}"
    );
}
