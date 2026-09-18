//! The proxy's own diagnostics must never stall the frames it forwards.
//!
//! `periculum` spawns `lora-proxy` with `RUST_LOG=debug` and
//! `stderr(Stdio::piped())`, and does not read that pipe until it kills the
//! proxy at scenario teardown (periculum `runner.rs`, `spawn_proxies` /
//! `kill_proxies`). A pipe holds ~64 KiB. At two `debug!` lines per KISS
//! frame the proxy reaches that in a couple of hundred frames — and if the
//! log writer blocks there, it blocks the single task that forwards frames
//! in BOTH directions. The board behind the proxy then goes deaf and mute
//! for the rest of the scenario.
//!
//! This test reproduces that: an undrained piped stderr, enough frames to
//! overflow it, and the requirement that every frame still comes out the
//! other side.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use leviculum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};

/// Frames to push through. Each one costs two `debug!` lines, so a few
/// hundred already exceed a pipe; 1500 leaves no doubt.
const FRAMES: usize = 1500;
const PAYLOAD_LEN: usize = 64;
const DEADLINE: Duration = Duration::from_secs(30);

struct ProxyChild(Child);

impl Drop for ProxyChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn open_pty(path: &std::path::Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOCTTY)
        .open(path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()))
}

#[test]
fn undrained_stderr_does_not_stall_forwarding() {
    let dir = std::env::temp_dir().join(format!("lora-proxy-logbp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let pty_a = dir.join("a.pty");
    let pty_b = dir.join("b.pty");

    // Spawned exactly the way periculum spawns it: debug logging into a
    // piped stderr that nobody reads while the proxy runs.
    let child = Command::new(env!("CARGO_BIN_EXE_lora-proxy"))
        .args([
            "virtual",
            "--pty-a",
            &pty_a.display().to_string(),
            "--pty-b",
            &pty_b.display().to_string(),
        ])
        .env("RUST_LOG", "debug")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lora-proxy");
    let _child = ProxyChild(child);

    let start = Instant::now();
    while !(pty_a.exists() && pty_b.exists()) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "proxy created no PTYs"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The symlink appears before the proxy enters its read loop.
    std::thread::sleep(Duration::from_millis(200));

    let mut side_a = open_pty(&pty_a);
    let mut side_b = open_pty(&pty_b);

    let received = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(VecDeque::<usize>::new()));

    let reader_received = Arc::clone(&received);
    let reader_seen = Arc::clone(&seen);
    std::thread::spawn(move || {
        let mut deframer = KissDeframer::with_max_payload(508);
        let mut buf = [0u8; 4096];
        while let Ok(n) = side_b.read(&mut buf) {
            if n == 0 {
                return;
            }
            for frame in deframer.process(&buf[..n]) {
                if let KissDeframeResult::Frame { payload, .. } = frame {
                    let idx = usize::from(payload[0]) | usize::from(payload[1]) << 8;
                    reader_seen.lock().expect("seen lock").push_back(idx);
                    reader_received.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    });

    // The writer needs its own thread: once the proxy stops reading, the PTY
    // fills and this write blocks, which must not hang the assertion below.
    std::thread::spawn(move || {
        let mut framed = Vec::new();
        for i in 0..FRAMES {
            let mut payload = vec![0u8; PAYLOAD_LEN];
            payload[0] = (i & 0xFF) as u8;
            payload[1] = ((i >> 8) & 0xFF) as u8;
            kiss::frame(kiss::CMD_DATA, &payload, &mut framed);
            if side_a.write_all(&framed).is_err() {
                return;
            }
            let _ = side_a.flush();
        }
    });

    let deadline = Instant::now() + DEADLINE;
    while received.load(Ordering::SeqCst) < FRAMES && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    let got = received.load(Ordering::SeqCst);
    let last = seen.lock().expect("seen lock").back().copied();
    assert_eq!(
        got, FRAMES,
        "forwarding stalled after {got} of {FRAMES} frames (last index forwarded: {last:?}) — \
         the proxy blocked writing its own diagnostics to an undrained stderr"
    );
}
