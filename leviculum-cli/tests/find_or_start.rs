//! `leviculum_std::instance::find_or_start` against the real lnsd
//! (zero-config step 4, #471): it starts one where none answers, finds one
//! where one does, the started daemon goes with its owner, and of two
//! callers that start at once one starts the daemon and the other finds it.
//!
//! Every test names its own instance, keyed by pid and test, so it never
//! meets a daemon of the host or of a parallel test; a named instance
//! without a configured port is asked on AF_UNIX only.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use leviculum_std::instance::{find_or_start, Instance, Report, Request, DAEMON_LOG};
use leviculum_std::interfaces::SharedInstanceTransport;

fn lnsd() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lnsd"))
}

fn write_config(dir: &Path, name: &str) {
    let mut f = std::fs::File::create(dir.join("config")).expect("write config");
    writeln!(
        f,
        "[reticulum]\n  enable_transport = False\n  share_instance = Yes\n  instance_name = {name}\n[logging]\n  loglevel = 4\n[interfaces]"
    )
    .expect("write config body");
}

fn request(dir: &Path, name: &str) -> Request {
    let mut r = Request::new(dir, name);
    r.lnsd = Some(lnsd());
    r.report = Report::Quiet;
    r
}

fn answers(name: &str) -> bool {
    SharedInstanceTransport::default().answers(name)
}

fn gone_within(name: &str, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if !answers(name) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_tool_without_a_daemon_starts_one_and_it_goes_with_the_tool() {
    let name = format!("fosstart{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &name);
    assert!(!answers(&name), "nothing may answer before the start");

    let instance = find_or_start(&request(dir.path(), &name)).expect("find_or_start");
    println!("{}", instance.line());
    assert!(
        instance.started(),
        "expected a start, got {}",
        instance.line()
    );
    assert!(answers(&name), "the started lnsd answers");
    assert!(
        dir.path().join(DAEMON_LOG).is_file(),
        "the daemon's output goes to the config directory"
    );

    drop(instance);
    assert!(
        gone_within(&name, Duration::from_secs(5)),
        "the daemon outlived its owner"
    );
}

#[test]
fn a_running_daemon_is_found_and_no_second_one_starts() {
    let name = format!("fosfind{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &name);
    let first = find_or_start(&request(dir.path(), &name)).expect("first");
    assert!(first.started());

    let second = find_or_start(&request(dir.path(), &name)).expect("second");
    println!("{}", second.line());
    assert!(
        matches!(second, Instance::Found { .. }),
        "the second caller must find the first's daemon, got {}",
        second.line()
    );
    assert_eq!(second.line(), "INSTANCE found=yes via=unix pid=none");
}

#[test]
fn two_callers_at_once_start_one_daemon() {
    let name = format!("fosrace{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &name);

    let barrier = Arc::new(Barrier::new(2));
    let callers: Vec<_> = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let dir = dir.path().to_path_buf();
            let name = name.clone();
            std::thread::spawn(move || {
                barrier.wait();
                find_or_start(&request(&dir, &name))
            })
        })
        .collect();
    let results: Vec<Instance> = callers
        .into_iter()
        .map(|c| c.join().expect("caller thread").expect("find_or_start"))
        .collect();
    for r in &results {
        println!("{}", r.line());
    }
    let started = results.iter().filter(|r| r.started()).count();
    assert_eq!(started, 1, "exactly one caller starts the daemon");
    assert!(answers(&name));
}

// A fresh directory (no file, instance `default`) is the cells' case,
// `zero_config_tool_starts_the_daemon` in periculum: binding `rns/default`
// here would collide with the host's own daemon and with other lanes.
