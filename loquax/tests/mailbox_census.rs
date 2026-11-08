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

    let stderr_ = Arc::clone(&stderr);
    tokio::task::spawn_blocking(move || wait_for(&stderr_, "HOME up", 30))
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
        .find(|line| line.contains("MAILBOX_CENSUS"))
        .unwrap_or_else(|| panic!("no MAILBOX_CENSUS line:\n{text}"));
    for column in loquax::home::census::COLUMNS {
        assert!(line.contains(&format!(" {column}=")), "{column}: {line}");
    }
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
