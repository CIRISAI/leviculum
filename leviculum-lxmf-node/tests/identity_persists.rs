//! The helper's LXMF address survives a restart of the helper (#322).
//!
//! What happened: the field base's `lxmf-node` (`/home/lew/feld`, default
//! `LXMF_STORAGE`) was restarted on 2026-09-26 and came back as a different
//! LXMF address — `4c64d723…` at 12:0x, `8f35c8d5…` at 23:55 — because the
//! delivery identity was drawn fresh in `LxmfHelperProcessor::new` on every
//! start while the storage directory kept only `transport_identity`. An
//! endpoint whose address changes when its host reboots is one nobody can
//! write to, and its operator had already been given the old one.
//!
//! This runs the shipped executable rather than the in-process harness, three
//! times against one shared instance, because the property under test is
//! exactly what a restart of the process does: what the second run of a
//! *process* reports, not what a second call of a constructor returns. The
//! in-process half — mint, reload, corrupt-is-fatal — is in
//! `src/identity.rs`'s unit tests.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::process::spawn_supervised;

/// The daemon config periculum renders for a rust node, cut to the one key
/// this test needs: the shared instance to join. Nothing here travels the
/// mesh, so the daemon carries no interfaces.
fn write_config(dir: &Path, instance_name: &str) {
    let mut file = std::fs::File::create(dir.join("config")).expect("create config");
    write!(
        file,
        "[reticulum]\n  \
         enable_transport = no\n  \
         share_instance = no\n  \
         instance_name = {instance_name}\n\n\
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

/// A line collector on its own thread, one per stream: an unread pipe stops
/// the child dead once the kernel buffer fills
/// (`tests/tracing_reaches_stderr.rs` says what that cost while it was being
/// written).
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

fn lines(stream: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    stream.lock().expect("collector mutex").clone()
}

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

/// What one run of the helper reported.
struct Run {
    /// `lxmf_ready`'s `hash=` — the LXMF address a correspondent writes to.
    hash: String,
    stderr: Vec<String>,
}

/// Start the helper once against `storage`, read its `lxmf_ready`, stop it the
/// way the driver does (`quit`), and insist it exited cleanly — a run that
/// crashed on the way out would leave the next one's storage in a state this
/// test was not asking about.
async fn run_helper(config_dir: &Path, storage: &Path, label: &str) -> Run {
    let mut helper_cmd = Command::new(env!("CARGO_BIN_EXE_lxmf-node"));
    helper_cmd
        .arg("--config")
        .arg(config_dir)
        .arg(label)
        .env("LXMF_STORAGE", storage)
        .env_remove("RUST_LOG")
        .env_remove("LEVICULUM_EVENT_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Supervised: the helper holds three pipes, so a `SIGKILL`ed test runner
    // has to take it along rather than leave it attached to the instance.
    let mut child = spawn_supervised(helper_cmd).expect("spawn the helper");
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
            "run {label} never reported lxmf_ready; its stderr was:\n{}",
            lines(&stderr_lines).join("\n")
        )
    });
    let hash = ready
        .split_whitespace()
        .find_map(|token| token.strip_prefix("hash="))
        .unwrap_or_else(|| panic!("lxmf_ready must carry its hash field: {ready}"))
        .to_string();

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
    assert!(
        status.is_some_and(|exit| exit.success()),
        "run {label} must exit cleanly on quit, got {status:?}; stderr:\n{}",
        stderr.join("\n")
    );

    Run { hash, stderr }
}

fn said(run: &Run, needle: &str) -> bool {
    run.stderr.iter().any(|line| line.contains(needle))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_against_the_same_storage_keeps_the_same_lxmf_address() {
    let instance = format!("lxmf-node-322-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
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

    // The deployment's shape: one durable storage directory, the process
    // started twice.
    let durable = tempfile::tempdir().expect("durable storage");
    let first = run_helper(config.path(), durable.path(), "field-base").await;
    let identity_file = durable
        .path()
        .join(leviculum_lxmf_node::identity::IDENTITY_FILE);
    assert!(
        identity_file.exists(),
        "the first start must leave the identity behind: {}",
        identity_file.display()
    );
    assert!(
        said(&first, "NEW address was created"),
        "a first start must say it minted an address:\n{}",
        first.stderr.join("\n")
    );

    let second = run_helper(config.path(), durable.path(), "field-base").await;
    assert_eq!(
        second.hash,
        first.hash,
        "a restart against the same storage must answer at the address its \
         operator was given; stderr of the second run:\n{}",
        second.stderr.join("\n")
    );
    assert!(
        said(&second, "lxmf identity loaded from"),
        "a restart must say it loaded, not minted:\n{}",
        second.stderr.join("\n")
    );

    // And the other half, which the test corpus depends on: nothing is shared
    // between storage directories, so a scenario's fresh container is still a
    // fresh peer.
    let fresh = tempfile::tempdir().expect("fresh storage");
    let third = run_helper(config.path(), fresh.path(), "fresh-peer").await;
    assert_ne!(
        third.hash, first.hash,
        "a fresh storage directory must be a fresh address"
    );

    daemon.stop().await.ok();
}
