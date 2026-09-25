//! mvr for Codeberg #418 — a slow event-log write stops announce handling.
//!
//! # The field measurement this reproduces
//!
//! The miauhaus soak node emits a `PATH_TABLE` liveness heartbeat every 10 s.
//! Over 49 days and 397 023 881 events, 2 928 of the 421 605 intervals between
//! consecutive heartbeats were longer than 11 s, with a tail to 37.0 s, and
//! during each of those the daemon emitted **nothing at all** — no packet, no
//! announce, not the heartbeat. Both long stalls that were pulled out of the
//! raw log have the same shape: the hole sits between the `ANN_RX` of one
//! announce and the `PATH_ADD` of that same destination.
//!
//! Nothing between those two emission sites can take seconds. The emission
//! itself can: the layer wrote each line with a blocking `write(2)` on the
//! thread that emitted it, under a process-global mutex, and the driver's
//! event loop emits while holding the core mutex. A `write(2)` that blocks
//! therefore stops the transport and, behind the core mutex, everything else.
//!
//! # What this reproduces, and how it is bounded
//!
//! One process, one transport, no network. The event log is a FIFO whose
//! reader this test throttles: `PROBE_FILL` events fill the pipe, the next
//! `write(2)` blocks until the reader takes bytes out, and the announce fed in
//! at that moment has its `ANN_RX` written before the block and its
//! `PATH_ADD` after it. That is the field's shape at millisecond scale.
//!
//! The stall is bounded by the reader, not by the code under test: the reader
//! pauses a fixed number of times and then drains at full speed, so even a
//! regression that blocks for ever cannot wedge the suite.
//!
//! # The two arms
//!
//! - `LEVICULUM_EVENT_LOG_SYNC=1` — the pre-fix sink, write on the emitting
//!   thread. This is the positive control: a harness that cannot show the
//!   stall proves nothing about a tree that does not stall.
//! - default — the queued sink, where the emitting thread hands the line to a
//!   writer thread and returns.
//!
//! The distribution both arms produce is printed (run with `--nocapture`);
//! the assertions are on the separation between them.

// A FIFO is the whole mechanism of this test; there is none off Unix
// (CIRIS fork: the Windows lane).
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Announces the probe feeds in, one fresh destination each.
const ROUNDS: usize = 4;
/// Pipe capacity. The Linux minimum is one page; a small pipe keeps the
/// filler the probe writes short and the whole measurement quick.
const PIPE_BYTES: libc::c_int = 4096;
/// The reader takes one pipe-full and then holds off this long. A sink write
/// that blocks therefore blocks for up to one of these.
const PAUSE: Duration = Duration::from_millis(300);
/// How many times it does that before draining at full speed. This is the
/// bound on the whole measurement: even a regression that blocks for ever
/// cannot wedge the suite, because after this the reader never pauses again.
const PAUSES: usize = 12;
/// An announce whose `process_incoming` takes at least this long counts as
/// stalled. Announce handling of one packet into an in-memory table is
/// microseconds; this is three orders of magnitude above it.
const STALL_MS: u64 = 150;
/// The queued arm must stay under this. Generous on purpose: what it measures
/// is in-memory table work plus a bounded enqueue, so anything near this is
/// already a scheduler artefact, and the arms are separated by more than the
/// slack.
const QUEUED_MAX_MS: u64 = 50;
/// Hard bound on one arm.
const ARM_TIMEOUT: Duration = Duration::from_secs(60);

fn probe_bin() -> PathBuf {
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
            .join("announce-stall-probe"),
        target_dir.join("debug").join("announce-stall-probe"),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--bin",
            "announce-stall-probe",
            "-p",
            "leviculum-std",
        ])
        .status()
        .expect("cargo build for announce-stall-probe");
    assert!(status.success(), "cargo build failed");
    candidates
        .iter()
        .find(|c| c.exists())
        .unwrap_or_else(|| panic!("announce-stall-probe not found after build"))
        .clone()
}

/// Owned FIFO path; removed on drop.
struct Fifo {
    path: PathBuf,
}

impl Fifo {
    fn new(dir: &Path, name: &str) -> Self {
        let path = dir.join(name);
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
        let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo {}: {}", path.display(), last_os_error());
        Fifo { path }
    }
}

impl Drop for Fifo {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn last_os_error() -> String {
    std::io::Error::last_os_error().to_string()
}

/// Open the FIFO for reading and drain it, pausing `PAUSES` times so the
/// writer's pipe fills. Returns every line it read.
///
/// The open blocks until the probe opens the write end, which is the
/// rendezvous this needs: the sink opens the file lazily, on its first event.
fn drain_throttled(path: PathBuf) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY) };
        assert!(fd >= 0, "open fifo for read: {}", last_os_error());
        // Shrink before the writer has had time to put more than a line in:
        // F_SETPIPE_SZ refuses a size below what is already buffered.
        unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, PIPE_BYTES) };

        let mut file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let mut text = String::new();
        let mut buf = [0u8; 8192];
        let mut pauses = 0usize;
        loop {
            if pauses < PAUSES {
                std::thread::sleep(PAUSE);
                pauses += 1;
            }
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(e) => panic!("read fifo: {e}"),
            }
        }
        text.lines().map(str::to_string).collect()
    })
}

/// `t=` of a canonical event line.
fn t_of(line: &str) -> Option<u64> {
    line.rsplit(' ')
        .next()?
        .strip_prefix("t=")
        .and_then(|v| v.parse().ok())
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .filter_map(|tok| tok.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Per destination, the interval between the `PKT_RX` that carried its
/// announce and the `PATH_ADD` that installed it — the span the field
/// measurement found a 27.6 s hole in, read out of the log the same way.
fn announce_gaps(lines: &[String]) -> Vec<u64> {
    let mut rx: BTreeMap<String, u64> = BTreeMap::new();
    let mut gaps = Vec::new();
    for line in lines {
        let Some(t) = t_of(line) else { continue };
        if line.starts_with("PKT_RX ") {
            if let Some(dst) = field(line, "dst") {
                rx.insert(dst.to_string(), t);
            }
        } else if line.starts_with("PATH_ADD ") {
            if let Some(dst) = field(line, "dst") {
                if let Some(start) = rx.remove(dst) {
                    gaps.push(t.saturating_sub(start));
                }
            }
        }
    }
    gaps
}

/// What one arm measured.
struct Arm {
    /// Wall time of each `process_incoming`, measured by the probe itself
    /// and reported out of band.
    rx_ms: Vec<u64>,
    /// `PKT_RX` to `PATH_ADD` per destination, read out of the event log.
    gaps: Vec<u64>,
    /// `ANN_SLOW` durations the transport's own instrumentation reported.
    ann_slow: Vec<u64>,
}

fn describe(arm: &str, a: &Arm) -> String {
    let mut sorted = a.rx_ms.clone();
    sorted.sort_unstable();
    let stalled = sorted.iter().filter(|m| **m >= STALL_MS).count();
    format!(
        "{arm}: n={} peak={}ms median={}ms >={STALL_MS}ms={} process_incoming={:?} \
         PKT_RX->PATH_ADD={:?} ANN_SLOW={:?}",
        sorted.len(),
        sorted.last().copied().unwrap_or(0),
        sorted.get(sorted.len() / 2).copied().unwrap_or(0),
        stalled,
        sorted,
        a.gaps,
        a.ann_slow,
    )
}

/// Run one arm.
fn run_arm(blocking: bool) -> Arm {
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = Fifo::new(dir.path(), "events.fifo");
    let report = dir.path().join("rx.txt");
    let reader = drain_throttled(fifo.path.clone());

    let mut cmd = Command::new(probe_bin());
    cmd.arg(ROUNDS.to_string())
        .arg(&report)
        .env("LEVICULUM_EVENT_LOG", &fifo.path)
        .env("LEVICULUM_EVENT_NODE", "probe")
        .env_remove("RUST_LOG");
    if blocking {
        cmd.env("LEVICULUM_EVENT_LOG_SYNC", "1");
    } else {
        cmd.env_remove("LEVICULUM_EVENT_LOG_SYNC");
    }
    // Supervised: the probe can sit for up to one `PAUSE` inside a blocking
    // `write(2)` on the FIFO, so a test binary that dies mid-arm would leave it
    // holding the pipe open with nothing left to drain it.
    let mut child = leviculum_std::process::spawn_supervised(cmd).expect("spawn probe");

    let deadline = Instant::now() + ARM_TIMEOUT;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(status.success(), "probe exited with {status}");
                break;
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("probe did not finish within {ARM_TIMEOUT:?}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    let lines = reader.join().expect("reader thread");
    let text = std::fs::read_to_string(&report).expect("probe report");
    let rx_ms: Vec<u64> = text
        .lines()
        .filter_map(|l| l.split(' ').nth(2))
        .filter_map(|v| v.parse::<u64>().ok())
        .map(|us| us / 1000)
        .collect();
    assert_eq!(rx_ms.len(), ROUNDS, "probe reported {text:?}");
    let ann_slow: Vec<u64> = lines
        .iter()
        .filter(|l| l.starts_with("ANN_SLOW "))
        .filter_map(|l| field(l, "ms").and_then(|v| v.parse().ok()))
        .collect();

    Arm {
        rx_ms,
        gaps: announce_gaps(&lines),
        ann_slow,
    }
}

#[test]
fn a_blocking_event_log_write_stalls_announce_handling() {
    let sync = run_arm(true);
    let queued = run_arm(false);

    println!("{}", describe("blocking (pre-#418 sink)", &sync));
    println!("{}", describe("queued   (post-#418 sink)", &queued));

    let sync_peak = sync.rx_ms.iter().copied().max().unwrap_or(0);
    let queued_peak = queued.rx_ms.iter().copied().max().unwrap_or(0);
    let sync_stalled = sync.rx_ms.iter().filter(|m| **m >= STALL_MS).count();

    // Positive control first: a green queued arm means nothing unless the
    // harness is shown to stall a tree that does stall.
    assert!(
        sync_stalled >= ROUNDS - 1 && sync_peak >= STALL_MS,
        "harness did not reproduce the stall on the blocking sink; {}",
        describe("blocking", &sync)
    );

    assert!(
        queued_peak < QUEUED_MAX_MS,
        "the queued sink still stalls announce handling: {} | {}",
        describe("queued", &queued),
        describe("blocking", &sync)
    );
    // A sink that stops stalling by losing the evidence is not a fix.
    assert_eq!(
        queued.gaps.len(),
        ROUNDS,
        "queued arm lost announce events: {}",
        describe("queued", &queued)
    );
}

/// Rounds for the overflow arm. Each announce emits a handful of events, so
/// this is comfortably more than the sink's 8192-line queue plus whatever the
/// pipe swallows before the writer thread blocks.
const OVERFLOW_ROUNDS: usize = 3000;
/// How long the reader refuses to take anything at all.
const FREEZE: Duration = Duration::from_millis(1500);

/// Open the FIFO and read nothing for `FREEZE`, then drain at full speed.
fn drain_after_freeze(path: PathBuf) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY) };
        assert!(fd >= 0, "open fifo for read: {}", last_os_error());
        let mut file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        std::thread::sleep(FREEZE);
        let mut text = String::new();
        let mut buf = [0u8; 8192];
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(e) => panic!("read fifo: {e}"),
            }
        }
        text.lines().map(str::to_string).collect()
    })
}

/// The queued sink buys its freedom from blocking with the possibility of
/// loss, so the loss has to be visible — otherwise the fix for a daemon that
/// goes deaf is a log that goes quiet, which is the same bug wearing a
/// different hat.
///
/// This is the positive control for that: a reader that takes nothing at all
/// while the probe emits far more than the queue holds, and the resulting
/// `EVENT_LOG_DROPPED` line with a non-zero count.
#[test]
fn an_overrun_queue_says_how_much_it_lost() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo = Fifo::new(dir.path(), "events.fifo");
    let report = dir.path().join("rx.txt");
    let reader = drain_after_freeze(fifo.path.clone());

    let status = Command::new(probe_bin())
        .arg(OVERFLOW_ROUNDS.to_string())
        .arg(&report)
        // No filling: the freeze alone is what backs the sink up here.
        .arg("0")
        .env("LEVICULUM_EVENT_LOG", &fifo.path)
        .env("LEVICULUM_EVENT_NODE", "probe")
        .env_remove("LEVICULUM_EVENT_LOG_SYNC")
        .env_remove("RUST_LOG")
        .status()
        .expect("run probe");
    assert!(status.success(), "probe exited with {status}");

    let lines = reader.join().expect("reader thread");
    let dropped: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("EVENT_LOG_DROPPED "))
        .collect();
    assert!(
        !dropped.is_empty(),
        "{OVERFLOW_ROUNDS} announces into a frozen sink lost nothing and said \
         nothing; {} lines came through",
        lines.len()
    );
    for l in &dropped {
        let n: u64 = field(l, "n")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("EVENT_LOG_DROPPED without a parseable n=: {l}"));
        assert!(n > 0, "EVENT_LOG_DROPPED reported n=0: {l}");
        assert!(
            t_of(l).is_some(),
            "EVENT_LOG_DROPPED is not a canonical line: {l}"
        );
    }
    println!(
        "overflow arm: {} lines survived, markers: {:?}",
        lines.len(),
        dropped
    );
}
