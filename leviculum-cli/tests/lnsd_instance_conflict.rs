//! What the operator actually reads when `lnsd` lands on a taken
//! instance name.
//!
//! The library-level guard for this lives in
//! `leviculum-std/tests/shared_instance_conflict.rs` and asserts the typed
//! error and its Display text. That test alone was not enough: it went
//! green while the binary still printed
//!
//! ```text
//! Error: SharedInstanceNameInUse { name: "realcollision" }
//! ```
//!
//! because returning `Err` from `main` prints the *Debug* form. The
//! sentence only reaches the journal if `main` formats with Display, and
//! nothing below the binary can prove that it does. Hence this test,
//! which runs the real executable via `CARGO_BIN_EXE_lnsd`.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

/// Minimal daemon config: no interfaces, no transport, just a shared
/// instance under `name`. The socket bind is the whole subject.
fn write_config(dir: &std::path::Path, name: &str) {
    let mut f = std::fs::File::create(dir.join("config")).expect("write config");
    writeln!(
        f,
        "[reticulum]\n  enable_transport = False\n  share_instance = Yes\n  instance_name = {name}\n[logging]\n  loglevel = 3\n[interfaces]"
    )
    .expect("write config body");
}

/// Poll until the daemon has taken the abstract socket, so the second
/// process meets a bound name rather than a race.
fn wait_for_instance(name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        // A missing `ss` is a missing tool, not a daemon that never bound:
        // swallowing the spawn error read as the latter on the forge, whose
        // image had no iproute2 (pipelines 476-488).
        let out = Command::new("ss")
            .arg("-xl")
            .output()
            .expect("ss runs (apt install iproute2)");
        if String::from_utf8_lossy(&out.stdout).contains(&format!("@rns/{name}")) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn lnsd_reports_a_taken_instance_name_in_words() {
    // Abstract sockets are per network namespace and outlive nothing, so
    // the name is keyed by pid to keep parallel runs apart.
    let name = format!("cliconflict{}", std::process::id());

    let dir_a = tempfile::tempdir().expect("temp dir");
    let dir_b = tempfile::tempdir().expect("temp dir");
    write_config(dir_a.path(), &name);
    write_config(dir_b.path(), &name);

    // Kills the holder however the assertions below turn out, so a failing
    // test does not leave a daemon squatting the name.
    struct Reaper(std::process::Child);
    impl Drop for Reaper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let mut holder = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    holder
        .arg("--config")
        .arg(dir_a.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _reaper = Reaper(spawn_supervised(holder).expect("first lnsd starts"));

    assert!(
        wait_for_instance(&name, Duration::from_secs(10)),
        "the first daemon never took @rns/{name}"
    );

    let second = Command::new(env!("CARGO_BIN_EXE_lnsd"))
        .arg("--config")
        .arg(dir_b.path())
        .output()
        .expect("second lnsd runs");

    assert!(
        !second.status.success(),
        "the second daemon must not start on a name that is already served"
    );

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains(&name),
        "stderr must name the instance, got: {stderr}"
    );
    assert!(
        stderr.contains("rnsd") || stderr.contains("lnsd"),
        "stderr must point at the daemon already holding it, got: {stderr}"
    );
    assert!(
        stderr.contains("instance_name"),
        "stderr must name the config key that resolves it, got: {stderr}"
    );
    assert!(
        !stderr.contains("AddrInUse") && !stderr.contains("SharedInstanceNameInUse {"),
        "the operator must get the sentence, not a Debug dump, got: {stderr}"
    );
}

/// Kills a daemon however the assertions turn out.
struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts `lnsd -c <dir>` with the given extra arguments, quiet.
fn start(dir: &std::path::Path, extra: &[&str]) -> Daemon {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("--config")
        .arg(dir)
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Daemon(spawn_supervised(cmd).expect("lnsd starts"))
}

/// 1638: a second lnsd under another instance name on the storage directory
/// a running lnsd uses ran a second daemon on the same files. It refuses now,
/// naming the running daemon's instance and pid.
#[test]
fn a_second_lnsd_under_another_name_on_the_same_storage_is_refused() {
    let first_name = format!("storagelock{}", std::process::id());
    let other_name = format!("storageother{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &first_name);

    let first = start(dir.path(), &[]);
    assert!(
        wait_for_instance(&first_name, Duration::from_secs(10)),
        "the first daemon never took @rns/{first_name}"
    );

    // Supervised and bounded: before 1638 the second daemon simply ran, and
    // an unbounded wait for its exit hung the test instead of failing it.
    let stderr_file = dir.path().join("second.stderr");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("--config")
        .arg(dir.path())
        .arg("--instance-name")
        .arg(&other_name)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_file).expect("stderr file"));
    let mut second = Daemon(spawn_supervised(cmd).expect("second lnsd spawns"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = second.0.try_wait().expect("wait on second lnsd") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stderr = std::fs::read_to_string(&stderr_file).unwrap_or_default();
    let status = status.unwrap_or_else(|| {
        panic!("the second daemon on a storage directory in use is still running after 15 s, stderr: {stderr}")
    });
    assert!(
        !status.success(),
        "the second daemon must not start on a storage directory in use, stderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!("instance {first_name}")),
        "stderr must name the running daemon's instance, got: {stderr}"
    );
    assert!(
        stderr.contains(&format!("pid {}", first.0.id())),
        "stderr must name the running daemon's pid, got: {stderr}"
    );
}

/// Control for the test above: the same second start on its own storage
/// directory runs.
#[test]
fn a_second_lnsd_under_another_name_on_its_own_storage_starts() {
    let first_name = format!("storagectl{}", std::process::id());
    let other_name = format!("storagectlother{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    let own = tempfile::tempdir().expect("temp dir");
    write_config(dir.path(), &first_name);

    let _first = start(dir.path(), &[]);
    assert!(
        wait_for_instance(&first_name, Duration::from_secs(10)),
        "the first daemon never took @rns/{first_name}"
    );

    let own_storage = own.path().to_string_lossy().into_owned();
    let _second = start(
        dir.path(),
        &["--instance-name", &other_name, "--storage", &own_storage],
    );
    assert!(
        wait_for_instance(&other_name, Duration::from_secs(10)),
        "the second daemon on its own storage never took @rns/{other_name}"
    );
}
