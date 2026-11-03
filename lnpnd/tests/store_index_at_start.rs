//! A start does not walk the message store (order 1376).
//!
//! lnpnd on miauhaus printed `IDENTITY` 130 s after it started, at 323 597
//! stored messages: the store's open listed the directory and opened every
//! file for its size and destination, and in that window the node answered
//! no link, so the mailbox's collects failed. The store now keeps what those
//! reads return in an index file inside the directory
//! (`leviculum_std::file_propagation_store`, "The index file"), written when
//! the start found it out of date and when the store is dropped.
//!
//! What only the binary can show: that the shutdown really drops the store,
//! so the next start reads no file, and that a damaged index file costs one
//! walk and nothing else. The field-size start is the ignored test at the
//! bottom, over a fixture store a disk has to hold.

use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::file_propagation_store::INDEX_FILE;
use leviculum_std::process::spawn_supervised;
use leviculum_std::ReticulumNode;

/// The line lnpnd prints once its propagation destination is up.
const READY: &str = "lnpnd: propagation node ready";

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

/// One lnpnd process and what it has said on stderr so far.
struct Lnpnd {
    child: std::process::Child,
    said: Arc<Mutex<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Lnpnd {
    fn spawn(config_dir: &Path, data_dir: &Path, instance: &str) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnpnd"));
        cmd.arg("--config")
            .arg(config_dir)
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--instance")
            .arg(instance)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = spawn_supervised(cmd).expect("the daemon binary runs");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let said = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&said);
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
        Self {
            child,
            said,
            reader: Some(reader),
        }
    }

    fn said(&self) -> String {
        self.said.lock().expect("stderr buffer lock").clone()
    }

    /// Wait for `line`, failing if the process exits first.
    async fn wait_for(&mut self, line: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        while !self.said().contains(line) {
            assert!(
                self.child.try_wait().expect("waitable").is_none(),
                "lnpnd exited before saying {line:?}; it said:\n{}",
                self.said()
            );
            assert!(
                Instant::now() < deadline,
                "lnpnd did not say {line:?} within {budget:?}; it said:\n{}",
                self.said()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// SIGTERM, as systemd stops the unit, and wait for a clean exit.
    async fn stop(mut self) -> String {
        let sent = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("run kill");
        assert!(sent.success());
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("waitable") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "lnpnd did not exit within 15 s of SIGTERM; it said:\n{}",
                self.said()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        if let Some(reader) = self.reader.take() {
            reader.join().expect("the stderr reader finishes");
        }
        let text = self.said();
        assert!(status.success(), "lnpnd must exit 0, got {status}:\n{text}");
        text
    }
}

impl Drop for Lnpnd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `<data_dir>/lxmf/messagestore`, where `lnpnd::message_store_dir` puts it.
fn store_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("lxmf").join("messagestore")
}

/// The start line that says what the store's open cost.
fn opened_line(text: &str) -> String {
    text.lines()
        .find(|line| line.starts_with("lnpnd: message store opened in"))
        .unwrap_or_else(|| panic!("no store-open line; lnpnd said:\n{text}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_index_file_survives_a_restart_and_a_damaged_one_costs_one_walk() {
    let instance = format!("lnpnd-index-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = start_daemon(&instance, daemon_storage.path()).await;
    let config_dir = tempfile::tempdir().expect("config dir");
    let data_dir = tempfile::tempdir().expect("data dir");

    // Three messages in our own name scheme, as a run before left them,
    // and the marker that says their stamp values were priced already.
    let store = store_dir(data_dir.path());
    std::fs::create_dir_all(&store).expect("store dir");
    // Received a minute ago: the first run's expiry sweep must leave them.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs();
    for n in 1..=3u8 {
        let name = format!("{}_{}_13_{n}", hex::encode([n; 32]), now - 60 + n as u64);
        let mut body = vec![n; 200];
        body[16..].fill(0xCD);
        std::fs::write(store.join(name), body).expect("message file");
    }
    std::fs::write(store.join(".stamp-values-true"), b"").expect("marker");

    let mut first = Lnpnd::spawn(config_dir.path(), data_dir.path(), &instance);
    first.wait_for(READY, Duration::from_secs(30)).await;
    assert!(
        opened_line(&first.said()).ends_with("index file missing, 0 from it, 3 file(s) read"),
        "{}",
        first.said()
    );
    // The start wrote it; take it away, so only the shutdown can bring it
    // back, and the next start shows whether it did.
    std::fs::remove_file(store.join(INDEX_FILE)).expect("the start wrote the index file");
    first.stop().await;
    assert!(
        store.join(INDEX_FILE).exists(),
        "the shutdown must drop the store, which writes the index file"
    );

    let mut second = Lnpnd::spawn(config_dir.path(), data_dir.path(), &instance);
    second.wait_for(READY, Duration::from_secs(30)).await;
    assert!(
        opened_line(&second.said()).ends_with("index file loaded, 3 from it, 0 file(s) read"),
        "{}",
        second.said()
    );
    second.stop().await;

    std::fs::write(store.join(INDEX_FILE), b"not an index").expect("damage it");
    let mut third = Lnpnd::spawn(config_dir.path(), data_dir.path(), &instance);
    third.wait_for(READY, Duration::from_secs(30)).await;
    let text = third.stop().await;
    assert!(
        opened_line(&text).ends_with("index file corrupt, 0 from it, 3 file(s) read"),
        "{text}"
    );
    assert!(
        text.contains("lnpnd: message store holds 3 message(s)"),
        "{text}"
    );

    let _ = daemon.stop().await;
}

/// The field-size start: a store of `LNPND_START_BENCH_STORE` (a directory,
/// for instance the 320 000-message fixture of
/// `file_propagation_store::tests::open_cost_at_field_scale`) opens, the
/// node comes up and answers a request on a link within 5 s of the spawn.
/// The request is `--status` on the control destination, a link and a
/// request to the same process the mailbox's collect links to.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a field-size store on a disk, named by LNPND_START_BENCH_STORE"]
async fn a_field_size_store_starts_and_answers_within_five_seconds() {
    let fixture = PathBuf::from(
        std::env::var("LNPND_START_BENCH_STORE").expect("LNPND_START_BENCH_STORE names a store"),
    );
    std::fs::write(fixture.join(".stamp-values-true"), b"").expect("marker");
    let instance = format!("lnpnd-field-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = start_daemon(&instance, daemon_storage.path()).await;
    let config_dir = tempfile::tempdir().expect("config dir");
    std::fs::write(
        config_dir.path().join("config"),
        "[propagation]\nmessage_storage_limit = 2000\n",
    )
    .expect("config");
    let data_dir = tempfile::tempdir().expect("data dir");
    std::fs::create_dir_all(data_dir.path().join("lxmf")).expect("lxmf dir");
    std::os::unix::fs::symlink(&fixture, store_dir(data_dir.path())).expect("link the fixture");

    for round in 0..2 {
        let spawned = Instant::now();
        let mut node = Lnpnd::spawn(config_dir.path(), data_dir.path(), &instance);
        node.wait_for(READY, Duration::from_secs(300)).await;
        let ready_ms = spawned.elapsed().as_millis();
        let status = Command::new(env!("CARGO_BIN_EXE_lnpnd"))
            .arg("--config")
            .arg(config_dir.path())
            .arg("--instance")
            .arg(&instance)
            .arg("--status")
            .output()
            .expect("run lnpnd --status");
        let answered_ms = spawned.elapsed().as_millis();
        let text = node.stop().await;
        println!(
            "START round={round} ready_ms={ready_ms} answered_ms={answered_ms} {}",
            opened_line(&text)
        );
        assert!(
            status.status.success(),
            "--status failed: {}\n{}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
        if round == 1 {
            assert!(opened_line(&text).contains("index file loaded"), "{text}");
            assert!(
                answered_ms < 5_000,
                "a start over the index file must answer within 5 s, took {answered_ms} ms"
            );
        }
    }
    let _ = daemon.stop().await;
}
