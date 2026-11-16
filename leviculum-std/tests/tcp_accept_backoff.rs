//! 1190: the TCP server's accept loop pauses after a failed accept instead of
//! retrying at once.
//!
//! On `EMFILE` the connection the kernel could not hand over stays queued, so
//! an immediate retry fails the same way: before 1190 the loop logged
//! `TCP accept error` and retried, a busy loop that wrote about 815 journal
//! lines per second on the public backbone and pushed every older entry off
//! the host. The loop now backs off from 10 ms to 1 s and reports once per
//! ten seconds (`TCP_ACCEPT_BACKOFF`).
//!
//! Its own test binary because it lowers the process's descriptor limit,
//! which would fail any test running beside it in the same process.

use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_std::driver::ReticulumNodeBuilder;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|p| p.into_inner())).into_owned()
    }
}

fn open_fds() -> libc::rlim_t {
    std::fs::read_dir("/proc/self/fd").unwrap().count() as libc::rlim_t
}

fn set_soft_nofile(n: libc::rlim_t) -> libc::rlim_t {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain syscalls on a stack value.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
        let old = lim.rlim_cur;
        lim.rlim_cur = n;
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &lim), 0);
        old
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exhausted_descriptor_table_pauses_the_accept_loop() {
    let captured = Captured::default();
    let sink = captured.clone();
    tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    let storage = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir");
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server_listener(listener)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build");
    node.start().await.expect("start");

    // The client ends exist before the limit drops: connecting takes no new
    // descriptor, accepting does.
    let clients: Vec<socket2::Socket> = (0..32)
        .map(|_| {
            socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
                .expect("socket")
        })
        .collect();
    let before = captured.text().len();
    let restored = set_soft_nofile(open_fds() + 1);
    for c in &clients {
        c.connect(&addr.into()).ok();
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    set_soft_nofile(restored);
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let log = captured.text()[before..].to_string();
    let backoff_lines = log.matches("TCP_ACCEPT_BACKOFF").count();
    let error_lines = log.matches("TCP accept error").count();
    let accepted = log.matches("Accepted connection").count();
    let summary = format!(
        "ACCEPT1190 backoff_lines={backoff_lines} error_lines={error_lines} accepted={accepted}"
    );
    println!("{summary}");
    node.stop().await.ok();
    drop(clients);

    assert_eq!(
        error_lines, 0,
        "one line per failed accept: {summary}\n{log}"
    );
    assert_eq!(
        backoff_lines, 1,
        "three seconds of EMFILE are one summary line: {summary}\n{log}"
    );
    assert!(
        log.contains("errors=1") && log.contains("fds="),
        "the line counts the errors and names the descriptor use: {log}"
    );
    // Once descriptors are back the queue drains: one source, so the first
    // sixteen are served and the rest meet the per-source cap.
    assert_eq!(
        accepted, 16,
        "once descriptors are back, the queued connections are accepted: {summary}\n{log}"
    );
    assert!(
        log.contains("TCP_SOURCE_CAPPED"),
        "the rest of the queue meets the per-source cap: {log}"
    );
    assert!(
        !log.contains("fds=?"),
        "a full table still names its descriptor use: {log}"
    );
}
