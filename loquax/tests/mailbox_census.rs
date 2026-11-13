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

/// The mailbox's line when it dials no public rendezvous (1785): an idle
/// mailbox has no network beyond its own daemon. On a host without
/// `/etc/leviculum/no-auto` (the forge runner) the shipped list was
/// dialled, the mesh's announces arrived, and each one kept the pump at
/// 20 ms for a second: 582 loop turns in 23 s against 180, CPU over the bar.
const NO_RENDEZVOUS: &str = "HOME rendezvous list=none ";

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
        // No public rendezvous on any host, marker or none (1785).
        .env(leviculum_std::auto_layer::ENV_NO_AUTO, "1")
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

/// One `/proc/<pid>/stat` style line's fields after the command's closing
/// parenthesis: utime and stime are the 14th and 15th of the line, the 12th
/// and 13th after it; minflt and majflt the 10th and 12th of the line.
fn stat_fields(stat: &str) -> Vec<u64> {
    let rest = &stat[stat.rfind(')').expect("comm") + 2..];
    rest.split_whitespace()
        .map(|field| field.parse::<u64>().unwrap_or(0))
        .collect()
}

/// User plus system CPU of `pid` in clock ticks, from `/proc/<pid>/stat`.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
    let fields = stat_fields(&stat);
    fields[11] + fields[12]
}

/// What a red idle window says about where the time went (1723): utime,
/// stime and the page faults of the whole process, and the CPU ticks of
/// each thread by its name. The gate's red on 8ae14da0 said only "54.41 %",
/// which could not tell a spinning thread from a process charged for the
/// host's page reclaim.
#[derive(Debug, Clone, Default)]
struct Breakdown {
    utime: u64,
    stime: u64,
    minflt: u64,
    majflt: u64,
    threads: std::collections::BTreeMap<String, u64>,
}

fn breakdown(pid: u32) -> Breakdown {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
    let fields = stat_fields(&stat);
    let mut threads = std::collections::BTreeMap::new();
    if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        for task in tasks.flatten() {
            let Ok(stat) = std::fs::read_to_string(task.path().join("stat")) else {
                continue;
            };
            let name = stat
                .find('(')
                .zip(stat.rfind(')'))
                .map(|(open, close)| stat[open + 1..close].to_string())
                .unwrap_or_default();
            let fields = stat_fields(&stat);
            let tid = task.file_name().to_string_lossy().into_owned();
            threads.insert(format!("{name}/{tid}"), fields[11] + fields[12]);
        }
    }
    Breakdown {
        utime: fields[11],
        stime: fields[12],
        minflt: fields[7],
        majflt: fields[9],
        threads,
    }
}

impl Breakdown {
    /// The window's growth from `before`, one line, busiest threads first.
    fn since(&self, before: &Breakdown) -> String {
        let mut threads: Vec<(String, u64)> = self
            .threads
            .iter()
            .map(|(name, ticks)| {
                let was = before.threads.get(name).copied().unwrap_or(0);
                (name.clone(), ticks.saturating_sub(was))
            })
            .filter(|(_, ticks)| *ticks > 0)
            .collect();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.1));
        format!(
            "utime={} stime={} minflt={} majflt={} threads=[{}]",
            self.utime.saturating_sub(before.utime),
            self.stime.saturating_sub(before.stime),
            self.minflt.saturating_sub(before.minflt),
            self.majflt.saturating_sub(before.majflt),
            threads
                .iter()
                .map(|(name, ticks)| format!("{name}:{ticks}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

/// `key=<n>` out of a `MAILBOX_CENSUS` line.
fn census_count(line: &str, key: &str) -> u64 {
    let needle = format!(" {key}=");
    let at = line
        .find(&needle)
        .unwrap_or_else(|| panic!("no {key} in {line}"));
    line[at + needle.len()..]
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{key} unreadable in {line}"))
}

/// The `n`th `MAILBOX_CENSUS` line on `stderr`, waiting up to `secs` for it.
fn nth_census(stderr: &Arc<Mutex<String>>, n: usize, secs: u64) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(line) = stderr
            .lock()
            .expect("stderr")
            .lines()
            .filter(|line| line.starts_with("MAILBOX_CENSUS "))
            .nth(n)
        {
            return line.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "no census line #{n} within {secs} s; stderr:\n{}",
            stderr.lock().expect("stderr")
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A census whose engines answered: SIGUSR1 through `signal`, then the next
/// `MAILBOX_CENSUS` line after the `seen` ones; a line printed when its
/// one-second wait passed without the engine's answer (`answered=0`, slow
/// under a CPU quota) carries no engine count, so the census is asked
/// again. Returns the line and the lines seen so far.
async fn answered_census(
    signal: &(dyn Fn(&str) + Sync),
    stderr: &Arc<Mutex<String>>,
    mut seen: usize,
) -> (String, usize) {
    for _ in 0..5 {
        signal("USR1");
        let stderr_ = Arc::clone(stderr);
        let line = tokio::task::spawn_blocking(move || nth_census(&stderr_, seen, 15))
            .await
            .expect("waited");
        seen += 1;
        if census_count(&line, "answered") > 0 {
            return (line, seen);
        }
    }
    panic!(
        "no census with the engine's answer in five asks; stderr:\n{}",
        stderr.lock().expect("stderr")
    )
}

/// Wakes per second an idle mailbox may spend, each count (1723). Idle, the
/// daemon loop wakes about six times a second (the quiet pump's 4, the
/// outbox tick and the outside look 1 each) and every engine ticks about 5
/// times (`POLL_INTERVAL_MS` 200 ms); the census itself adds up to 50 busy
/// pumps in its one-second answer wait. A loop or a deadline that spins
/// wakes thousands of times a second, so the bar sits far from both.
const IDLE_WAKES_PER_SEC: u64 = 40;

/// Order 1585: an idle mailbox does no work. The field mailbox burned 23 %
/// of a core with nothing to do (6 h 18 min in 28 h). Measured, not
/// assumed: the mailbox's CPU over [`IDLE_WINDOW`] after it came up, with
/// no device, no message and no group, against [`IDLE_CPU_PERCENT`]. On
/// the coder host the 20 ms pump measured 0.6 % and 1.0 %, the quiet pump
/// 0.35 % twice; the ceiling sits between them, so a pump that never
/// quiets is likely, not certain, to be caught.
///
/// Order 1723: the same window counted in wakes, which the host's speed
/// does not move. A census before and one after it give the daemon loop's
/// `turns` and the engines' `engine_ticks`, each held to
/// [`IDLE_WAKES_PER_SEC`]. And a red CPU bar names where the time went,
/// utime against stime, faults and threads.
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
        // No public rendezvous on any host, marker or none (1785).
        .env(leviculum_std::auto_layer::ENV_NO_AUTO, "1")
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
    let signal = move |name: &str| {
        let status = Command::new("kill")
            .args(["-s", name, &pid.to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill -s {name} {pid}");
    };
    let stderr_ = Arc::clone(&stderr);
    tokio::task::spawn_blocking(move || wait_for(&stderr_, SIGNALS_HANDLED, 30))
        .await
        .expect("waited");
    {
        let held = stderr.lock().expect("stderr");
        assert!(
            held.contains(NO_RENDEZVOUS),
            "the idle mailbox dials a rendezvous, so it is not idle; stderr:\n{held}"
        );
    }
    // The start's own work settles first.
    tokio::time::sleep(Duration::from_secs(3)).await;
    // The wakes before the window; the census's own busy second passes
    // before the CPU is read.
    let (first, seen) = answered_census(&signal, &stderr, 0).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let ticks_per_sec = 100.0; // USER_HZ on Linux
    let before = cpu_ticks(pid);
    let parts_before = breakdown(pid);
    let started = Instant::now();
    tokio::time::sleep(IDLE_WINDOW).await;
    let used = (cpu_ticks(pid) - before) as f64 / ticks_per_sec;
    let parts = breakdown(pid).since(&parts_before);
    let elapsed = started.elapsed().as_secs_f64();
    let percent = 100.0 * used / elapsed;
    let (second, _) = answered_census(&signal, &stderr, seen).await;
    let counted_secs = (Instant::now() - started).as_secs() + 3;
    // The counts only grow; a smaller second would be the test's own error.
    let turns = census_count(&second, "turns")
        .checked_sub(census_count(&first, "turns"))
        .unwrap_or_else(|| panic!("turns went back: {first} then {second}"));
    let engine_ticks = census_count(&second, "engine_ticks")
        .checked_sub(census_count(&first, "engine_ticks"))
        .unwrap_or_else(|| panic!("engine_ticks went back: {first} then {second}"));
    eprintln!(
        "MAILBOX_IDLE_CPU percent={percent:.2} cpu_s={used:.2} window_s={} elapsed_s={elapsed:.2} {parts}",
        IDLE_WINDOW.as_secs()
    );
    eprintln!("MAILBOX_IDLE_WAKES turns={turns} engine_ticks={engine_ticks} secs={counted_secs}");
    signal("TERM");
    let _ = tokio::task::spawn_blocking(move || child.wait()).await;
    reader.join().expect("reader");
    let bar = IDLE_WAKES_PER_SEC * counted_secs;
    assert!(
        turns > 0 && turns <= bar,
        "the idle mailbox's loop woke {turns} times in {counted_secs} s (bar {bar})"
    );
    assert!(
        engine_ticks > 0 && engine_ticks <= bar,
        "the idle mailbox's engine ticked {engine_ticks} times in {counted_secs} s (bar {bar})"
    );
    assert!(
        percent < IDLE_CPU_PERCENT,
        "an idle mailbox used {percent:.2} % of a core over {} s ({elapsed:.2} s elapsed): {parts}",
        IDLE_WINDOW.as_secs()
    );
}
