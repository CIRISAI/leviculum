//! The helper's stack has a voice: what `leviculum-core` and `leviculum-std`
//! say with `tracing` must reach the helper's stderr (#330).
//!
//! The gap this pins, measured on a saved conformance run before the fix:
//! 1602 stderr lines from a `leviculum` LXMF helper, every one of them
//! `^\[lxmf-node\]`, and not one `INFO|DEBUG|WARN|leviculum_core|
//! leviculum_std` among them — `main.rs` installed no `tracing` subscriber at
//! all. The two lines a scenario needs in order to assert that our terminus
//! adopted a link-request proof's hop count (`LRPROOF hop asymmetry at link
//! terminus`, `node/link_management.rs`, and `event=PATH_REBALANCE`,
//! `transport.rs`) are emitted by the core, so on the helper they went
//! nowhere and the regression cell could only assert their absence.
//!
//! This runs the shipped executable rather than the in-process harness the
//! other tests here use: installing a global subscriber is a property of the
//! process, and in-process the test runner's own subscriber (or absence of
//! one) would decide the outcome.
//!
//! Both halves are asserted, because the fix is only worth having if the
//! driver's channel is untouched: the core's line on stderr, `EVENT` lines
//! and nothing else on stdout.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_std::driver::ReticulumNodeBuilder;

/// The daemon config periculum renders for a rust node, cut to the keys this
/// test needs: the shared instance to join, and `loglevel = 5`, which is
/// verbatim what every scenario's config carries (`periculum/src/topology.rs`,
/// `loglevel = 5`). Nothing here sets `RUST_LOG` — the point is that the
/// config file alone takes the helper to debug, so a scenario needs neither a
/// new argv nor a new environment variable to get the `event=` lines.
fn write_config(dir: &Path, instance_name: &str) {
    let mut file = std::fs::File::create(dir.join("config")).expect("create config");
    write!(
        file,
        "[reticulum]\n  \
         enable_transport = no\n  \
         share_instance = no\n  \
         instance_name = {instance_name}\n\n\
         [logging]\n  loglevel = 5\n\n\
         [interfaces]\n"
    )
    .expect("write config");
}

/// Kills the helper however the assertions turn out, so a failing test leaves
/// no process attached to the instance.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A line collector on its own thread, one per stream.
///
/// Both streams have to be drained while the helper runs: an unread pipe stops
/// the child dead once the kernel buffer fills, and the `tracing` writer is
/// synchronous, so at `loglevel = 5` that would wedge the stack mid-startup
/// (it did, while this test was being written). The lines are kept rather than
/// consumed, because every assertion below is "did this ever appear".
fn collect<R: std::io::Read + Send + 'static>(stream: R) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let collected = Arc::clone(&seen);
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            collected.lock().expect("collector mutex").push(line);
        }
    });
    seen
}

/// Every line seen on a stream so far.
fn lines(stream: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    stream.lock().expect("collector mutex").clone()
}

/// Poll `stream` until one of its lines satisfies `want`, and return it.
async fn wait_for_line<F>(
    stream: &Arc<Mutex<Vec<String>>>,
    budget: Duration,
    want: F,
) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    let deadline = Instant::now() + budget;
    loop {
        if let Some(line) = lines(stream).into_iter().find(|line| want(line)) {
            return Some(line);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cores_tracing_lines_reach_the_helpers_stderr() {
    let instance = format!("lxmf-node-330-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    // The daemon the helper joins. No interfaces: nothing in this test travels
    // the mesh, what is under test happens between the helper's own stack and
    // its two output streams.
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(daemon_storage.path().to_path_buf())
        .build()
        .await
        .expect("build the shared-instance daemon");
    daemon.start().await.expect("start the daemon");

    let config = tempfile::tempdir().expect("helper config");
    write_config(config.path(), &instance);
    let storage = tempfile::tempdir().expect("helper storage");

    let mut child = Command::new(env!("CARGO_BIN_EXE_lxmf-node"))
        .arg("--config")
        .arg(config.path())
        .arg("alice")
        .env("LXMF_STORAGE", storage.path())
        // The assertion is about the level the config file computes, not an
        // ambient one inherited from the test runner.
        .env_remove("RUST_LOG")
        .env_remove("LEVICULUM_EVENT_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the helper");
    let mut stdin = child.stdin.take().expect("stdin piped");
    let stdout_lines = collect(child.stdout.take().expect("stdout piped"));
    let stderr_lines = collect(child.stderr.take().expect("stderr piped"));
    let mut helper = Reaper(child);

    // `lxmf_ready` arrives on the first tick after the node attaches; the
    // budget is for the attach, not for any work.
    let ready = wait_for_line(&stdout_lines, Duration::from_secs(20), |line| {
        line.starts_with("EVENT lxmf_ready ")
    })
    .await
    .unwrap_or_else(|| {
        panic!(
            "the helper never reported lxmf_ready; its stderr was:\n{}",
            lines(&stderr_lines).join("\n")
        )
    });
    let hash = ready
        .split_whitespace()
        .find_map(|token| token.strip_prefix("hash="))
        .unwrap_or_else(|| panic!("lxmf_ready must still carry its hash field: {ready}"))
        .to_string();

    // One announce: the cheapest command that makes the CORE do something
    // this process can see. Everything before it is `leviculum_std` wiring.
    writeln!(stdin, "announce").expect("write the announce command");
    stdin.flush().expect("flush the announce command");
    assert!(
        wait_for_line(&stdout_lines, Duration::from_secs(10), |line| line
            .starts_with("EVENT lxmf_announce_sent "))
        .await
        .is_some(),
        "the helper did not announce; its stderr was:\n{}",
        lines(&stderr_lines).join("\n")
    );

    // The fix: the core's own `tracing` output, target and all, on stderr —
    // and about this helper's own destination, not about the harness.
    //
    // `PKT_TX` also shows the second-order gain: the journey correlator is
    // computed only when its target is enabled (`transport.rs`,
    // `pkt_journey_enabled` → `tracing::enabled!`), which with no subscriber
    // installed was never, so `periculum trace` had nothing to follow on a
    // leviculum helper either.
    let core_line = wait_for_line(&stderr_lines, Duration::from_secs(10), |line| {
        line.contains("leviculum_core") && line.contains("PKT_TX")
    })
    .await
    .unwrap_or_else(|| {
        panic!(
            "no leviculum_core tracing line on the helper's stderr; it said:\n{}",
            lines(&stderr_lines).join("\n")
        )
    });

    writeln!(stdin, "quit").expect("write the quit command");
    stdin.flush().expect("flush the quit command");
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut status = None;
    while Instant::now() < deadline {
        match helper.0.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(_) => break,
        }
    }

    let stderr = lines(&stderr_lines);
    let stdout = lines(&stdout_lines);

    // The helper's own diagnostics are unaffected: they shared the stream
    // before the subscriber existed and still do.
    assert!(
        stderr
            .iter()
            .any(|line| line.contains("[lxmf-node] shared-instance client connected")),
        "the helper's own diagnostics must still be on stderr:\n{}",
        stderr.join("\n")
    );

    // And the driver's channel did not move. `install_global_subscriber`'s
    // stderr writer exists to keep log lines off stdout; here that stream is
    // parsed token by token by the driver, so anything but an `EVENT` line on
    // it is a broken scenario, not a cosmetic blemish.
    let intruders: Vec<&String> = stdout
        .iter()
        .filter(|line| !line.is_empty() && !line.starts_with("EVENT "))
        .collect();
    assert!(
        intruders.is_empty(),
        "stdout must carry EVENT lines only; found {intruders:?}\ncore line was: {core_line}"
    );
    assert!(
        stdout
            .iter()
            .any(|line| line.starts_with("EVENT lxmf_ready ") && line.contains(&hash)),
        "lxmf_ready must be unchanged on stdout, hash and all:\n{}",
        stdout.join("\n")
    );
    assert!(
        status.is_some_and(|exit| exit.success()),
        "the helper must still exit cleanly on quit, got {status:?}; stderr:\n{}",
        stderr.join("\n")
    );

    daemon.stop().await.ok();
}
