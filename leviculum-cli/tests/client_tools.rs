//! What the path and probe clients do against a running daemon, end to
//! end.
//!
//! The tool is the `path_query` half of Codeberg #173: query a path, wait
//! for it, drop it. Two properties cannot be shown below the binary and
//! are therefore asserted here on the real executable:
//!
//! 1. **`-w` governs the wait.** Periculum #20 is the counter-example —
//!    `rnprobe` steps give up after roughly 10 s no matter what timeout
//!    was configured, which makes a scenario red for a reason unrelated
//!    to the property it tests. A tool whose wait window is a constant
//!    passes any single-timeout test, so the assertion here is
//!    *differential*: the same query with a short and a long `-w` must
//!    differ in elapsed time by about the difference between them.
//!
//! 2. **`-d` reaches the daemon's table, not the client's own.** The
//!    client process exits immediately afterwards, so dropping its own
//!    copy would be a no-op that still prints success. The second drop of
//!    the same path is what distinguishes the two: it can only fail if
//!    the first one removed something the daemon still holds.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

// The host-wide listener-port allocator every suite that spawns a daemon
// shares; see its own module docs for why a private per-binary counter is
// not good enough. It is std-only, so a `#[path]` include costs nothing.
#[path = "../../leviculum-std/tests/support/port_alloc.rs"]
#[allow(dead_code)]
mod port_alloc;

/// A hash no destination in these topologies has, so a path to it is
/// never found however long the tool waits.
const UNKNOWN_HASH: &str = "00112233445566778899aabbccddeeff";

/// Kills its daemon however the assertions turn out, so a failing test
/// leaves no process squatting an instance name or a port.
struct Reaper(std::process::Child);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write_config(dir: &Path, instance_name: &str, interface_ini: &str) {
    std::fs::create_dir_all(dir.join("storage")).expect("storage dir");
    let mut f = std::fs::File::create(dir.join("config")).expect("write config");
    write!(
        f,
        "[reticulum]\n  \
         enable_transport = yes\n  \
         share_instance = yes\n  \
         instance_name = {instance_name}\n  \
         respond_to_probes = yes\n\n\
         [logging]\n  loglevel = 4\n\n\
         [interfaces]\n{interface_ini}\n"
    )
    .expect("write config body");
}

fn spawn_lnsd(dir: &Path) -> Reaper {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("-v")
        .arg("--config")
        .arg(dir)
        .stdout(Stdio::from(
            std::fs::File::create(dir.join("lnsd-stdout.log")).expect("stdout log"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(dir.join("lnsd-stderr.log")).expect("stderr log"),
        ));
    Reaper(spawn_supervised(cmd).expect("spawn lnsd"))
}

/// Poll until the daemon has taken its abstract socket, so a client meets
/// a bound name rather than a race.
fn wait_for_instance(name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(out) = Command::new("ss").arg("-xl").output() {
            if String::from_utf8_lossy(&out.stdout).contains(&format!("@rns/{name}")) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

struct Run {
    code: i32,
    stdout: String,
    elapsed: Duration,
}

fn lnpath(config: &Path, args: &[&str]) -> Run {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_lnpath"))
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .expect("run lnpath");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        elapsed: started.elapsed(),
    }
}

/// The `\r`-and-spaces overwrite rnpath uses to clear its spinner line
/// leaves the terminal showing only the last segment; a parser wants the
/// same. Mirrors Periculum's bridge sanitiser.
fn last_segment(stdout: &str) -> String {
    stdout
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .rsplit('\r')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[test]
fn the_timeout_argument_governs_the_wait() {
    let name = format!("lnpathto{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &name, "");
    let _daemon = spawn_lnsd(dir.path());
    assert!(
        wait_for_instance(&name, Duration::from_secs(20)),
        "lnsd never bound rns/{name}"
    );

    let short = lnpath(dir.path(), &["-w", "2", UNKNOWN_HASH]);
    let long = lnpath(dir.path(), &["-w", "8", UNKNOWN_HASH]);

    assert_eq!(short.code, 1, "no path is exit 1; stdout: {}", short.stdout);
    assert_eq!(long.code, 1, "no path is exit 1; stdout: {}", long.stdout);
    assert_eq!(last_segment(&short.stdout), "Path not found");
    assert_eq!(last_segment(&long.stdout), "Path not found");

    // Printed unconditionally, not only on failure: a delta of about six
    // seconds is the measurement that makes the assertion below mean
    // something, and a reader of the gate log should not have to make the
    // test red to see it.
    let delta_ms = long.elapsed.as_millis() as i128 - short.elapsed.as_millis() as i128;
    println!(
        "PATH_WAIT short_w=2 short_ms={} long_w=8 long_ms={} delta_ms={}",
        short.elapsed.as_millis(),
        long.elapsed.as_millis(),
        delta_ms
    );

    // Startup is a constant in both runs, so it cancels in the difference.
    // A tool with a hardcoded window (the #20 defect) fails here even
    // though each run on its own looks plausible.
    let delta = long.elapsed.as_secs_f64() - short.elapsed.as_secs_f64();
    assert!(
        (4.0..=9.0).contains(&delta),
        "-w must govern the wait: -w 2 took {:.1} s, -w 8 took {:.1} s (delta {:.1} s)",
        short.elapsed.as_secs_f64(),
        long.elapsed.as_secs_f64(),
        delta
    );
    assert!(
        short.elapsed < Duration::from_secs(7),
        "-w 2 waited {:.1} s",
        short.elapsed.as_secs_f64()
    );

    // Dropping a path that was never there is a failure, with the
    // reference tool's sentence.
    let drop = lnpath(dir.path(), &["-d", UNKNOWN_HASH]);
    assert_eq!(drop.code, 1, "stdout: {}", drop.stdout);
    assert_eq!(
        last_segment(&drop.stdout),
        format!("Unable to drop path to <{UNKNOWN_HASH}>. Does it exist?")
    );
}

#[test]
fn a_path_is_found_then_dropped_from_the_daemons_table() {
    let port = port_alloc::free_tcp_port();
    let name_a = format!("lnpathsrv{}", std::process::id());
    let name_b = format!("lnpathcli{}", std::process::id());
    let dir_a = tempfile::tempdir().expect("temp dir");
    let dir_b = tempfile::tempdir().expect("temp dir");

    write_config(
        dir_a.path(),
        &name_a,
        &format!(
            "  [[Peer]]\n    type = TCPServerInterface\n    enabled = yes\n    \
             listen_ip = 127.0.0.1\n    listen_port = {port}\n    \
             ingress_control = false\n"
        ),
    );
    write_config(
        dir_b.path(),
        &name_b,
        &format!(
            "  [[Peer]]\n    type = TCPClientInterface\n    enabled = yes\n    \
             target_host = 127.0.0.1\n    target_port = {port}\n    \
             ingress_control = false\n"
        ),
    );

    let _a = spawn_lnsd(dir_a.path());
    let _b = spawn_lnsd(dir_b.path());
    assert!(wait_for_instance(&name_a, Duration::from_secs(20)));
    assert!(wait_for_instance(&name_b, Duration::from_secs(20)));

    // A's probe responder is a destination A owns and answers path
    // requests for, so B can reach it without waiting for an announce
    // cadence. `lnstatus` is where its hash is readable from outside.
    let hash = probe_hash(dir_a.path());

    // The TCP peering has to be up before the path request goes out;
    // lnpath's own -w window then covers the request itself.
    std::thread::sleep(Duration::from_secs(3));

    let found = lnpath(dir_b.path(), &["-w", "20", &hash]);
    let line = last_segment(&found.stdout);
    assert_eq!(found.code, 0, "stdout: {}", found.stdout);
    assert!(
        line.starts_with(&format!("Path found, destination <{hash}> is "))
            && line.contains(" hop")
            && line.contains(" away via <")
            && line.contains(" on "),
        "unparseable path line: {line:?}"
    );

    let dropped = lnpath(dir_b.path(), &["-d", &hash]);
    assert_eq!(dropped.code, 0, "stdout: {}", dropped.stdout);
    assert_eq!(
        last_segment(&dropped.stdout),
        format!("Dropped path to <{hash}>")
    );

    // The drop reached the daemon, not a copy inside the exited client:
    // the second one finds nothing left to remove.
    let again = lnpath(dir_b.path(), &["-d", &hash]);
    assert_eq!(again.code, 1, "stdout: {}", again.stdout);
    assert_eq!(
        last_segment(&again.stdout),
        format!("Unable to drop path to <{hash}>. Does it exist?")
    );
}

/// Read the daemon's probe-responder hash out of `lnstatus`, which prints
/// it as ` Probe responder at <hash> active`.
fn probe_hash(config: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let out = Command::new(env!("CARGO_BIN_EXE_lnstatus"))
            .arg("--config")
            .arg(config)
            .output()
            .expect("run lnstatus");
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(rest) = text.split("Probe responder at <").nth(1) {
            if let Some(hash) = rest.split('>').next() {
                if hash.len() == 32 {
                    return hash.to_string();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("lnstatus never reported a probe responder");
}

#[test]
fn lnprobe_gets_a_reply_from_the_daemons_own_probe_responder() {
    // Phase 1 of scripts/lnprobe-accept.sh, automated: the daemon answers
    // probes for a destination it owns, and the client reports the
    // round-trip in the reference tool's words.
    let name = format!("lnprobersp{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &name, "");
    let _daemon = spawn_lnsd(dir.path());
    assert!(
        wait_for_instance(&name, Duration::from_secs(20)),
        "lnsd never bound rns/{name}"
    );
    let hash = probe_hash(dir.path());

    let out = Command::new(env!("CARGO_BIN_EXE_lnprobe"))
        .arg("--config")
        .arg(dir.path())
        .args(["-t", "20", "rnstransport.probe", &hash])
        .output()
        .expect("run lnprobe");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stdout: {stdout}");
    assert!(
        stdout.contains(&format!("Valid reply from <{hash}>")),
        "no valid reply in: {stdout:?}"
    );
    assert!(
        stdout.contains("Round-trip time is ") && stdout.contains(" hop"),
        "no round-trip line in: {stdout:?}"
    );
    assert!(
        stdout.contains("Sent 1, received 1, packet loss 0.0%"),
        "no summary line in: {stdout:?}"
    );
}
