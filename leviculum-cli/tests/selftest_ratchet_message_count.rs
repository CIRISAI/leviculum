//! `--messages` sizes the ratchet exchange, and nothing periculum parses moves.
//!
//! Codeberg #429. The ratchet modes of `lnstest selftest` sent a count this
//! file's subject can now choose: `--duration` and `--rate` size Phase 5 and
//! Phase 8, which `--mode ratchet-*` never enters, so a periculum cell had no
//! way to buy more than the 10 per direction the tool fixed. At n = 20 the
//! interval a run's own counts support is ~25 points wide, which leaves the
//! 80 % floor of `regression/selftest_ratchet_direct.toml` undecided for 19
//! of the 20 possible imperfect runs (periculum #43); 40 per direction is
//! what that cell needs.
//!
//! Why the real binary and not `run_selftest` in-process: the count is only
//! worth anything if it reaches the summary line periculum reads off stdout
//! (`periculum/src/bridge.rs`, `parse_delivery_measurement`, via
//! `executor.rs::scan_delivery_measurements`), and that line is a `println!`.
//! So the assertion is on the process's own output, in the shape the harness
//! parses it: `sent=<N> recv=<M> (<P>%)`.
//!
//! The default arm is not decoration. A flag whose default moved would
//! silently re-size every existing invocation, log line and cell that does
//! not pass it, so `sent=20` without the flag is asserted alongside
//! `sent=80` with it.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

// The host-wide listener-port allocator every suite that spawns a daemon
// shares; see its own module docs for why a private per-binary counter is
// not good enough.
#[path = "../../leviculum-std/tests/support/port_alloc.rs"]
#[allow(dead_code)]
mod port_alloc;

/// Messages per direction the tool sends without the flag, and the count
/// every existing caller depends on.
const DEFAULT_PER_DIRECTION: u64 = 10;

/// What the cell of #429 asks for.
const CHOSEN_PER_DIRECTION: u64 = 40;

/// Kills its daemon however the assertions turn out, so a failing test
/// leaves no process squatting an instance name or a port.
struct Reaper(std::process::Child);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write_config(dir: &Path, instance_name: &str, port: u16) {
    std::fs::create_dir_all(dir.join("storage")).expect("storage dir");
    let mut f = std::fs::File::create(dir.join("config")).expect("write config");
    write!(
        f,
        "[reticulum]\n  \
         enable_transport = yes\n  \
         share_instance = yes\n  \
         instance_name = {instance_name}\n  \
         respond_to_probes = no\n\n\
         [logging]\n  loglevel = 4\n\n\
         [interfaces]\n  \
         [[Peer]]\n    type = TCPServerInterface\n    enabled = yes\n    \
         listen_ip = 127.0.0.1\n    listen_port = {port}\n    \
         ingress_control = false\n"
    )
    .expect("write config body");
}

fn spawn_lnsd(dir: &Path) -> Reaper {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("--config")
        .arg(dir)
        .stdout(Stdio::from(
            std::fs::File::create(dir.join("lnsd-stdout.log")).expect("stdout log"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(dir.join("lnsd-stderr.log")).expect("stderr log"),
        ));
    Reaper(spawn_supervised(cmd).expect("spawn lnsd"))
}

/// Poll until the relay's TCP port answers, so the tool's own pre-check is
/// not a race against the daemon's start.
fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// The `sent=` / `recv=` / `(P%)` triple off one summary line, parsed the way
/// periculum's leviculum bridge parses it: digits after each key, then the
/// first parenthesised percentage after `recv=`. A shape change that would
/// break the harness breaks this helper first.
fn parse_summary(line: &str) -> Option<(u64, u64, String)> {
    let after_sent = &line[line.find("sent=")? + 5..];
    let sent_end = after_sent
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_sent.len());
    let sent: u64 = after_sent[..sent_end].parse().ok()?;

    let after_recv = &line[line.find("recv=")? + 5..];
    let recv_end = after_recv
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_recv.len());
    let recv: u64 = after_recv[..recv_end].parse().ok()?;

    let rest = &after_recv[recv_end..];
    let open = rest.find('(')?;
    let close_rel = rest[open + 1..].find("%)")?;
    let pct = rest[open + 1..open + 1 + close_rel].trim().to_string();
    pct.parse::<f64>().ok()?;
    Some((sent, recv, pct))
}

/// One `lnstest selftest --mode ratchet-basic` run against the relay, with
/// `--messages N` when `messages` is given. Returns the whole stdout.
fn run_ratchet_basic(port: u16, messages: Option<u64>) -> String {
    let target = format!("127.0.0.1:{port}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnstest"));
    cmd.arg("selftest").arg(&target).arg(&target).args([
        "--mode",
        "ratchet-basic",
        "--discovery-timeout",
        "60",
    ]);
    if let Some(n) = messages {
        cmd.args(["--messages", &n.to_string()]);
    }
    let out = cmd.output().expect("run lnstest selftest");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    println!(
        "--- lnstest --messages {messages:?} (exit {:?}) ---",
        out.status.code()
    );
    println!("{stdout}");
    stdout
}

/// The summary of the ratchet phase, which is the line carrying both `sent=`
/// and the mode's name.
fn ratchet_summary(stdout: &str) -> (u64, u64, String) {
    let line = stdout
        .lines()
        .find(|l| l.contains("Ratchet ratchet-basic:") && l.contains("sent="))
        .unwrap_or_else(|| panic!("no ratchet summary line in:\n{stdout}"));
    parse_summary(line)
        .unwrap_or_else(|| panic!("summary line no longer parses as periculum reads it: {line:?}"))
}

#[test]
fn the_messages_flag_sizes_the_ratchet_exchange_and_defaults_to_ten() {
    let port = port_alloc::free_tcp_port();
    let name = format!("lnsmsg{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &name, port);
    let _daemon = spawn_lnsd(dir.path());
    assert!(
        wait_for_port(port, Duration::from_secs(20)),
        "lnsd never listened on 127.0.0.1:{port}"
    );

    let default_run = run_ratchet_basic(port, None);
    let chosen_run = run_ratchet_basic(port, Some(CHOSEN_PER_DIRECTION));

    let (default_sent, default_recv, default_pct) = ratchet_summary(&default_run);
    let (chosen_sent, chosen_recv, chosen_pct) = ratchet_summary(&chosen_run);

    // Printed unconditionally: these four numbers are the measurement the
    // assertions rest on, and a reader of the gate log should not have to
    // make the test red to see them.
    println!(
        "RATCHET_COUNT default_sent={default_sent} default_recv={default_recv} \
         default_pct={default_pct} chosen_sent={chosen_sent} chosen_recv={chosen_recv} \
         chosen_pct={chosen_pct}"
    );

    assert_eq!(
        default_sent,
        DEFAULT_PER_DIRECTION * 2,
        "without --messages the tool must still send {DEFAULT_PER_DIRECTION} each \
         direction, or every existing invocation and periculum cell changed size"
    );
    assert_eq!(
        chosen_sent,
        CHOSEN_PER_DIRECTION * 2,
        "--messages {CHOSEN_PER_DIRECTION} must send {CHOSEN_PER_DIRECTION} each \
         direction"
    );

    // Loopback through one relay: the cell's three clean-channel steps sit at
    // 100 % there, so a shortfall here is a defect and not a threshold
    // question. Asserted on both runs, because a count that only delivers at
    // n = 20 would buy the cell nothing.
    assert_eq!(
        default_recv, default_sent,
        "loopback lost packets at the default count: {default_recv}/{default_sent}"
    );
    assert_eq!(
        chosen_recv, chosen_sent,
        "loopback lost packets at {CHOSEN_PER_DIRECTION} each direction: \
         {chosen_recv}/{chosen_sent}"
    );

    // The phase's own announcement agrees with what it sent, so a reader of a
    // log knows the count before the summary arrives.
    assert!(
        default_run.contains(&format!(
            "sending {DEFAULT_PER_DIRECTION} messages each direction"
        )),
        "default run did not announce its count"
    );
    assert!(
        chosen_run.contains(&format!(
            "sending {CHOSEN_PER_DIRECTION} messages each direction"
        )),
        "--messages run did not announce its count"
    );
}

#[test]
fn a_mode_that_does_not_read_the_flag_says_so() {
    // Not a daemon test: the note is printed before the first TCP pre-check,
    // and an unreachable target is enough to reach it.
    let out = Command::new(env!("CARGO_BIN_EXE_lnstest"))
        .arg("selftest")
        .args(["127.0.0.1:1", "--mode", "bulk-transfer", "--messages", "40"])
        .output()
        .expect("run lnstest selftest");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--messages sizes --mode ratchet-basic and ratchet-enforced only"),
        "a flag that sizes nothing must say so, not be swallowed; stdout:\n{stdout}"
    );
}

#[test]
fn zero_messages_is_an_argument_error_and_not_a_zero_percent_verdict() {
    let out = Command::new(env!("CARGO_BIN_EXE_lnstest"))
        .arg("selftest")
        .args(["127.0.0.1:1", "--mode", "ratchet-basic", "--messages", "0"])
        .output()
        .expect("run lnstest selftest");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.contains("invalid --messages 0"),
        "expected the argument error, got stderr:\n{stderr}"
    );
}
