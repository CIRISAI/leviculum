//! The mailbox's SIGUSR1 census (order 1585) through the real binary:
//! `loquax mailbox run` beside a shared instance, one SIGUSR1, and the dump
//! on stderr.
//!
//! Before 1585 the signal killed the mailbox (its default disposition), so
//! a field mailbox at 630 MiB could not say which table held it. Now the
//! process lives on, prints a `MAILBOX_CENSUS` line naming every column of
//! `loquax::home::census::COLUMNS`, the home node's transport census under
//! `--- node home ---`, and lnsd's process sections with the `gap:` line
//! over one total.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod support;

const LOQUAX: &str = env!("CARGO_BIN_EXE_loquax");

/// The mailbox's line once its SIGUSR1 and SIGTERM handlers are installed.
const SIGNALS_HANDLED: &str = "HOME signals SIGUSR1=census";

/// Wait until `stderr` holds `needle`, or panic with what it holds.
fn wait_for(stderr: &Arc<Mutex<String>>, needle: &str, secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if stderr.lock().expect("stderr").contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no {needle:?} within {secs} s; stderr:\n{}",
            stderr.lock().expect("stderr")
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sigusr1_prints_the_census_and_the_mailbox_lives_on() {
    let daemon = support::daemon("census", |b| b).await;
    let home = tempfile::tempdir().expect("loquax home");
    let mut command = Command::new(LOQUAX);
    command
        .args(["mailbox", "run", "--instance", &daemon.instance, "--config"])
        .arg(daemon.config_dir.path())
        .args(["--sync-interval", "off"])
        .env("LNMSG_HOME", home.path())
        .env_remove("LEVICULUM_EVENT_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn the mailbox");
    let stderr = Arc::new(Mutex::new(String::new()));
    let reader = {
        let stderr = Arc::clone(&stderr);
        let pipe = child.stderr.take().expect("stderr pipe");
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut held = stderr.lock().expect("stderr");
                held.push_str(&line);
                held.push('\n');
            }
        })
    };
    let pid = child.id().to_string();
    let signal = |name: &str| {
        let status = Command::new("kill")
            .args(["-s", name, &pid])
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill -s {name} {pid}");
    };

    // The daemon's own word that SIGUSR1 is handled (1691). `HOME up` came
    // too early, and as a substring it matched `HOME upstream`, logged
    // before the handler: a slow forge signalled into the default
    // disposition and the mailbox died of signal 10 with no dump.
    let stderr_ = Arc::clone(&stderr);
    tokio::task::spawn_blocking(move || wait_for(&stderr_, SIGNALS_HANDLED, 30))
        .await
        .expect("waited");
    signal("USR1");
    let stderr_ = Arc::clone(&stderr);
    tokio::task::spawn_blocking(move || wait_for(&stderr_, "gap:", 15))
        .await
        .expect("waited");
    // The signal is the census's, not the end of the process.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "the mailbox died of SIGUSR1; stderr:\n{}",
        stderr.lock().expect("stderr")
    );

    let text = stderr.lock().expect("stderr").clone();
    let line = text
        .lines()
        .find(|line| line.starts_with("MAILBOX_CENSUS "))
        .unwrap_or_else(|| panic!("no MAILBOX_CENSUS line:\n{text}"));
    for column in loquax::home::census::COLUMNS {
        assert!(line.contains(&format!(" {column}=")), "{column}: {line}");
    }
    // And the structured event, for the log's readers.
    assert!(
        text.contains("occasion") && text.contains("\"signal\""),
        "{text}"
    );
    assert!(line.contains(" nodes=1 "), "{line}");
    assert!(text.contains("--- node home ---"), "{text}");
    assert!(text.contains("=== Total estimated: "), "{text}");
    assert!(text.contains("known_identities: "), "{text}");
    // The dump, for the reader of a green run.
    eprintln!("{text}");

    signal("TERM");
    let status = tokio::task::spawn_blocking(move || child.wait())
        .await
        .expect("waited")
        .expect("wait");
    assert!(status.success(), "{status:?}");
    reader.join().expect("reader");
}

/// User plus system CPU of `pid` in clock ticks, from `/proc/<pid>/stat`.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
    // The fields after the command's closing parenthesis; utime and stime
    // are the 14th and 15th of the line, the 12th and 13th after it.
    let rest = &stat[stat.rfind(')').expect("comm") + 2..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields[11].parse::<u64>().expect("utime") + fields[12].parse::<u64>().expect("stime")
}

/// Order 1585: an idle mailbox does no work. The field mailbox burned 23 %
/// of a core with nothing to do (6 h 18 min in 28 h). Measured, not
/// assumed: the mailbox's CPU over [`IDLE_WINDOW`] after it came up, with
/// no device, no message and no group, against [`IDLE_CPU_PERCENT`]. On
/// the coder host the 20 ms pump measured 0.6 % and 1.0 %, the quiet pump
/// 0.35 % twice; the ceiling sits between them, so a pump that never
/// quiets is likely, not certain, to be caught.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_mailbox_uses_almost_no_cpu() {
    const IDLE_WINDOW: Duration = Duration::from_secs(20);
    const IDLE_CPU_PERCENT: f64 = 0.8;
    let daemon = support::daemon("idle", |b| b).await;
    let home = tempfile::tempdir().expect("loquax home");
    let mut command = Command::new(LOQUAX);
    command
        .args(["mailbox", "run", "--instance", &daemon.instance, "--config"])
        .arg(daemon.config_dir.path())
        .args(["--sync-interval", "off"])
        .env("LNMSG_HOME", home.path())
        .env_remove("LEVICULUM_EVENT_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn the mailbox");
    let stderr = Arc::new(Mutex::new(String::new()));
    let reader = {
        let stderr = Arc::clone(&stderr);
        let pipe = child.stderr.take().expect("stderr pipe");
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let mut held = stderr.lock().expect("stderr");
                held.push_str(&line);
                held.push('\n');
            }
        })
    };
    let pid = child.id();
    let stderr_ = Arc::clone(&stderr);
    tokio::task::spawn_blocking(move || wait_for(&stderr_, SIGNALS_HANDLED, 30))
        .await
        .expect("waited");
    // The start's own work settles first.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let ticks_per_sec = 100.0; // USER_HZ on Linux
    let before = cpu_ticks(pid);
    let started = Instant::now();
    tokio::time::sleep(IDLE_WINDOW).await;
    let used = (cpu_ticks(pid) - before) as f64 / ticks_per_sec;
    let percent = 100.0 * used / started.elapsed().as_secs_f64();
    eprintln!(
        "MAILBOX_IDLE_CPU percent={percent:.2} cpu_s={used:.2} window_s={}",
        IDLE_WINDOW.as_secs()
    );
    let status = Command::new("kill")
        .args(["-s", "TERM", &pid.to_string()])
        .status()
        .expect("kill");
    assert!(status.success());
    let _ = tokio::task::spawn_blocking(move || child.wait()).await;
    reader.join().expect("reader");
    assert!(
        percent < IDLE_CPU_PERCENT,
        "an idle mailbox used {percent:.2} % of a core over {} s",
        IDLE_WINDOW.as_secs()
    );
}
