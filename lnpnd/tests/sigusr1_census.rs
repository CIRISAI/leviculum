//! SIGUSR1 prints lnpnd's memory census in the shape lnsd prints its own.
//!
//! The eight-hour soak (periculum `soak/lnpnd_eight_hours`, 2026-10-09)
//! could read lnsd's measured live bytes but only VmData for lnpnd, so the
//! growth it saw could not be told apart into tables and leak (550).
//! periculum's census parser (`periculum/src/soak.rs`, `parse_last_dump`)
//! reads `=== Total estimated: N bytes ===`, `=== Mappings: N ...` and the
//! `gap: rss ...` line, taking the live bytes from `; - live N = ...` and
//! `None` from `; - live n/a;`. This test holds lnpnd to exactly those
//! lines, in both builds:
//!
//! * plain: the dump is there and its gap line says `live n/a`, as lnsd's
//!   does without the feature;
//! * `--features heap-accounting`: the gap line subtracts a measured live
//!   count, the `=== Measured live: ...` line follows, and with
//!   `LEVICULUM_HEAP_SAMPLE` set the sampler's table does too.
//!
//! The daemon is the real binary against a real in-process shared
//! instance: what is under test is the process's response to a signal.

use std::io::Read as _;
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::process::spawn_supervised;
use leviculum_std::ReticulumNode;

/// The line lnpnd prints once its propagation destination is up.
const READY: &str = "lnpnd: propagation node ready";
/// The line lnpnd prints on the clean shutdown path.
const SHUTDOWN: &str = "lnpnd: shutting down";
/// periculum's `DUMP_GAP_PREFIX`: one per dump.
const GAP: &str = "gap: rss ";

/// The shared-instance daemon lnpnd attaches to.
async fn start_daemon(instance_name: &str, storage: &Path) -> ReticulumNode {
    let daemon_tcp: SocketAddr = "127.0.0.1:0".parse().expect("loopback address");
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.to_string())
        .add_tcp_server(daemon_tcp)
        .storage_path(storage.to_path_buf())
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");
    daemon
}

/// The daemon child, killed and reaped however the test leaves.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn kill(signal: &str, pid: u32) {
    let sent = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .expect("run kill");
    assert!(sent.success(), "kill -{signal} failed: {sent}");
}

/// The number after `label` on a gap line, the way periculum's `gap_term`
/// reads it; `None` where the line says `n/a`.
fn gap_term(line: &str, label: &str) -> Option<u64> {
    let (_, rest) = line.split_once(label)?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Start lnpnd, send it SIGUSR1 once it is ready, and return everything it
/// said up to its clean shutdown on SIGTERM.
async fn census_after_sigusr1(dump_complete: impl Fn(&str) -> bool) -> String {
    let instance_name = format!("lnpnd-usr1-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let _daemon = start_daemon(&instance_name, daemon_storage.path()).await;

    let config_dir = tempfile::tempdir().expect("lnpnd config dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnpnd"));
    cmd.arg("--config")
        .arg(config_dir.path())
        .arg("--instance")
        .arg(&instance_name)
        // Read only by an instrumented build; a plain one ignores it, which
        // is part of what the plain case asserts.
        .env("LEVICULUM_HEAP_SAMPLE", "16")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = Reaped(spawn_supervised(cmd).expect("the daemon binary runs"));

    let mut stderr = child.0.stderr.take().expect("piped stderr");
    let collected = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&collected);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .expect("stderr buffer lock")
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    let said = || collected.lock().expect("stderr buffer lock").clone();

    let wait_for = |what: &str, done: &dyn Fn(&str) -> bool, child: &mut Reaped| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let text = said();
            if done(&text) {
                return;
            }
            assert!(
                child.0.try_wait().expect("the child is waitable").is_none(),
                "lnpnd exited while waiting for {what}; it said:\n{text}"
            );
            assert!(
                Instant::now() < deadline,
                "lnpnd never printed {what}; it said:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    wait_for("its ready line", &|t| t.contains(READY), &mut child);
    assert!(
        !said().contains(GAP),
        "a dump before any SIGUSR1; it said:\n{}",
        said()
    );
    kill("USR1", child.0.id());
    wait_for("the census", &dump_complete, &mut child);
    // The dump must not have been the default disposition's last act.
    assert!(
        child.0.try_wait().expect("the child is waitable").is_none(),
        "SIGUSR1 ended lnpnd; it said:\n{}",
        said()
    );

    kill("TERM", child.0.id());
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("the child is waitable") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "lnpnd did not exit within 15 s of SIGTERM; it said:\n{}",
            said()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    reader.join().expect("the stderr reader finishes");
    let text = said();
    assert!(
        status.success() && text.contains(SHUTDOWN),
        "lnpnd must still shut down cleanly after a census, got {status}; it said:\n{text}"
    );
    text
}

/// The lines periculum's parser needs from any build, exactly once.
fn assert_census_shape(text: &str) -> String {
    let gaps: Vec<&str> = text
        .lines()
        .filter(|l| l.trim_start().starts_with(GAP))
        .collect();
    assert_eq!(gaps.len(), 1, "one SIGUSR1, one dump; it said:\n{text}");
    for section in [
        "=== Total estimated: ",
        "=== Process RSS: ",
        "=== Mappings: ",
    ] {
        let line = text
            .lines()
            .find(|l| l.starts_with(section))
            .unwrap_or_else(|| panic!("no `{section}` line; it said:\n{text}"));
        let value = line[section.len()..].split_whitespace().next();
        assert!(
            value.and_then(|v| v.parse::<u64>().ok()).is_some(),
            "`{line}` carries no number"
        );
    }
    let gap = gaps[0].trim().to_string();
    assert!(
        gap.rsplit_once("; unaccounted ")
            .and_then(|(_, v)| v.trim().parse::<u64>().ok())
            .is_some(),
        "gap line without an unaccounted figure: {gap}"
    );
    gap
}

#[cfg(not(feature = "heap-accounting"))]
#[tokio::test(flavor = "multi_thread")]
async fn sigusr1_prints_the_census_with_live_n_a() {
    let text = census_after_sigusr1(|t| t.lines().any(|l| l.starts_with(GAP))).await;
    let gap = assert_census_shape(&text);
    assert!(gap.contains("; - live n/a;"), "{gap}");
    assert_eq!(gap_term(&gap, "- live "), None, "{gap}");
    assert!(
        !text.contains("=== Measured live: ") && !text.contains("=== Heap sample: "),
        "a plain build has nothing measured to print; it said:\n{text}"
    );
}

#[cfg(feature = "heap-accounting")]
#[tokio::test(flavor = "multi_thread")]
async fn sigusr1_prints_measured_live_bytes_and_the_sampler() {
    let text = census_after_sigusr1(|t| {
        t.contains("=== Measured live: ") && t.contains("=== Heap sample: ")
    })
    .await;
    let gap = assert_census_shape(&text);
    let live = gap_term(&gap, "- live ").unwrap_or_else(|| panic!("live not measured: {gap}"));
    assert!(live > 0, "{gap}");
    assert!(gap.contains("(model, not subtracted)"), "{gap}");
    let measured = text
        .lines()
        .find(|l| l.starts_with("=== Measured live: "))
        .unwrap_or_default();
    assert!(measured.contains("allocator system"), "{measured}");
    let measured_bytes = measured["=== Measured live: ".len()..]
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<u64>().ok());
    assert!(
        measured_bytes.is_some_and(|b| b > 0),
        "the shim is lnpnd's global allocator, so it counted something: {measured}"
    );
    assert!(
        text.contains("Heap accounting: on"),
        "the startup line names the build; it said:\n{text}"
    );
}

/// `--version` tells the instrumented build from the plain one.
#[test]
fn version_names_the_heap_accounting_feature() {
    let out = Command::new(env!("CARGO_BIN_EXE_lnpnd"))
        .arg("--version")
        .output()
        .expect("lnpnd --version runs");
    assert!(out.status.success());
    let version = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        version.contains("+heapacct"),
        cfg!(feature = "heap-accounting"),
        "{version}"
    );
}
