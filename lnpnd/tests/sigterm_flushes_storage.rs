//! `systemctl stop` and `restart` must not cost the daemon what it learned.
//!
//! systemd stops a unit with SIGTERM. lnpnd used to wait on `ctrl_c()`
//! alone, i.e. SIGINT, so a SIGTERM ended it by the default disposition:
//! no shutdown line, no `node.stop()`, and so no flush of the node storage.
//! `known_destinations` is flush-on-shutdown by design
//! (`leviculum-std/src/storage.rs`), with an hourly crash-protection flush
//! behind it, so every restart of a daemon that had run for less than an
//! hour forgot every destination it had learned, and with them the
//! app_data #420 recalls across a restart.
//!
//! Both signals are run through the same steps. SIGINT is the positive
//! control: it always took the clean path, so if it fails here the harness
//! is broken, not the daemon.
//!
//! The daemon is the real binary against a real in-process shared
//! instance; what is under test is the process's response to a signal,
//! which nothing in-process can stand in for.

use std::io::Read as _;
use std::net::SocketAddr;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::process::spawn_supervised;
use leviculum_std::ReticulumNode;
use rand_core::OsRng;

/// The line lnpnd prints once its propagation destination is up.
const READY: &str = "lnpnd: propagation node ready";
/// The line lnpnd prints on the clean shutdown path.
const SHUTDOWN: &str = "lnpnd: shutting down";

/// The shared-instance daemon lnpnd attaches to.
async fn start_daemon(instance_name: &str, storage: &Path) -> ReticulumNode {
    // `:0`: the kernel picks the port, nothing dials this server; the test
    // is wired entirely over the shared instance.
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
        // No-ops once the child has been reaped below; std refuses to signal
        // a `Child` it has already waited on.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Where lnpnd's node storage keeps `known_destinations` with the default
/// data dir: `main.rs` hands `node_builder` `<data_dir>/storage`, and the
/// default data dir is `<configdir>/storage`.
fn known_destinations(config_dir: &Path) -> PathBuf {
    config_dir
        .join("storage")
        .join("storage")
        .join("known_destinations")
}

/// Start lnpnd, let it learn one destination, send it `signal` (a name
/// `kill` understands), and check that it took the clean shutdown path.
async fn signal_ends_cleanly(signal: &str) {
    let instance_name = format!("lnpnd-sig{signal}-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let daemon = start_daemon(&instance_name, daemon_storage.path()).await;

    let config_dir = tempfile::tempdir().expect("lnpnd config dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnpnd"));
    cmd.arg("--config")
        .arg(config_dir.path())
        .arg("--instance")
        .arg(&instance_name)
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

    let deadline = Instant::now() + Duration::from_secs(30);
    while !said().contains(READY) {
        assert!(
            child.0.try_wait().expect("the child is waitable").is_none(),
            "lnpnd exited before it was ready; it said:\n{}",
            said()
        );
        assert!(
            Instant::now() < deadline,
            "lnpnd never became ready; it said:\n{}",
            said()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Something for the node storage to flush: a destination lnpnd learns
    // from an announce. Without it the flush is skipped as having nothing
    // dirty, and the file's absence would prove nothing either way.
    let peer = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "lnpndsigterm",
        &["peer"],
    )
    .expect("create destination");
    let peer_hash = *peer.hash();
    daemon.register_destination(peer);
    daemon
        .announce_destination(&peer_hash, Some(b"recall me"))
        .await
        .expect("announce the peer");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The file carries the peer only once the node storage has been
    // flushed. Compared by content rather than by mtime: tmpfs stamps files
    // from the coarse clock, which runs milliseconds behind
    // `SystemTime::now()`, so a write after a mark can carry an earlier time.
    let file = known_destinations(config_dir.path());
    let holds_peer = |path: &Path| {
        std::fs::read(path).is_ok_and(|bytes| {
            bytes
                .windows(peer_hash.as_bytes().len())
                .any(|window| window == peer_hash.as_bytes())
        })
    };
    assert!(
        !holds_peer(&file),
        "{} already holds the peer before the signal, so the run cannot \
         show what the shutdown wrote",
        file.display()
    );

    let sent = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(child.0.id().to_string())
        .status()
        .expect("run kill");
    assert!(sent.success(), "kill -{signal} failed: {sent}");

    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("the child is waitable") {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "lnpnd did not exit within 15 s of SIG{signal}; it said:\n{}",
            said()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    reader.join().expect("the stderr reader finishes");
    let text = said();

    assert_eq!(
        status.signal(),
        None,
        "SIG{signal} must be handled, not end lnpnd by the default \
         disposition; it said:\n{text}"
    );
    assert!(
        status.success(),
        "lnpnd must exit 0 on SIG{signal}, got {status}; it said:\n{text}"
    );
    assert!(
        text.contains(SHUTDOWN),
        "SIG{signal} must take the shutdown path; it said:\n{text}"
    );
    assert!(
        holds_peer(&file),
        "SIG{signal} must flush the node storage: {} does not hold the \
         destination lnpnd learned in this run (exists: {})",
        file.display(),
        file.exists()
    );
}

/// The positive control: SIGINT always ran the clean path.
#[tokio::test(flavor = "multi_thread")]
async fn sigint_shuts_down_and_flushes_the_node_storage() {
    signal_ends_cleanly("INT").await;
}

/// The bug: SIGTERM, which systemd sends, must do the same.
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_shuts_down_and_flushes_the_node_storage() {
    signal_ends_cleanly("TERM").await;
}
