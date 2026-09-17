//! `storage_path` in the config file has to reach the storage that is
//! actually opened — in the daemon and in the client tools alike
//! (Codeberg #241).
//!
//! Both halves are observable from outside the process, and only from
//! outside: what the config says is a path, and the evidence is which
//! directory ends up holding `transport_identity`. A unit test on the
//! resolver cannot see that the binary then ignores it, which is exactly
//! how this shipped — `lnsd` overwrote the parsed value with
//! `<config_dir>/storage` one line after loading it, and every client
//! tool passed the same derived path to the builder.
//!
//! Observed in production: a transport node whose config pointed at an
//! external disk generated a fresh identity under `/etc/reticulum/storage`
//! and rejoined the public mesh under a new address.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_std::process::spawn_supervised;

/// Minimal daemon config: no interfaces, no transport, no shared instance
/// — only the storage path under test.
fn write_config(dir: &Path, storage: &Path) {
    let mut f = std::fs::File::create(dir.join("config")).expect("write config");
    writeln!(
        f,
        "[reticulum]\n  enable_transport = False\n  share_instance = No\n  storage_path = {}\n[logging]\n  loglevel = 3\n[interfaces]",
        storage.display()
    )
    .expect("write config body");
}

/// Same, but with a shared instance the client tools can address.
fn write_shared_config(dir: &Path, storage: &Path, name: &str) {
    let mut f = std::fs::File::create(dir.join("config")).expect("write config");
    writeln!(
        f,
        "[reticulum]\n  enable_transport = False\n  share_instance = Yes\n  instance_name = {name}\n  storage_path = {}\n[logging]\n  loglevel = 3\n[interfaces]",
        storage.display()
    )
    .expect("write config body");
}

/// Poll until the daemon has taken the abstract socket, so the client meets a
/// bound name rather than a race.
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

/// Poll until `path` exists, so the assertions meet a finished write
/// rather than a race with daemon startup.
fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Kills the daemon however the assertions turn out.
struct Reaper(std::process::Child);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn lnsd_keeps_its_identity_where_the_config_says() {
    let dir = tempfile::tempdir().expect("temp dir");
    let external = tempfile::tempdir().expect("temp dir");
    let storage = external.path().join("node-storage");
    write_config(dir.path(), &storage);

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("--config")
        .arg(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _reaper = Reaper(spawn_supervised(cmd).expect("lnsd starts"));

    assert!(
        wait_for_file(&storage.join("transport_identity"), Duration::from_secs(10)),
        "lnsd never wrote an identity under the configured storage_path {}",
        storage.display()
    );
    assert!(
        !dir.path().join("storage").exists(),
        "lnsd created {} although the config named another storage path",
        dir.path().join("storage").display()
    );
}

#[test]
fn lnsd_storage_flag_overrides_the_config() {
    let dir = tempfile::tempdir().expect("temp dir");
    let external = tempfile::tempdir().expect("temp dir");
    let from_config = external.path().join("from-config");
    let from_flag = external.path().join("from-flag");
    write_config(dir.path(), &from_config);

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("--config")
        .arg(dir.path())
        .arg("--storage")
        .arg(&from_flag)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _reaper = Reaper(spawn_supervised(cmd).expect("lnsd starts"));

    assert!(
        wait_for_file(
            &from_flag.join("transport_identity"),
            Duration::from_secs(10)
        ),
        "--storage did not win over the config's storage_path"
    );
    assert!(
        !from_config.join("transport_identity").exists(),
        "the config's storage_path was used although --storage was given"
    );
}

#[test]
fn a_client_tool_does_not_leave_a_decoy_identity_beside_the_config() {
    // The second half of #241: a client derived its storage from the config
    // directory too, so it generated its own `transport_identity` under
    // `<config_dir>/storage`. That file then won the daemon-authkey lookup
    // ahead of the real one and `lnstatus` reported `authentication failed`.
    //
    // No daemon runs here: the client builds its node (and its storage)
    // before it ever reaches the shared-instance socket, which is what makes
    // the decoy observable in isolation.
    let dir = tempfile::tempdir().expect("temp dir");
    let external = tempfile::tempdir().expect("temp dir");
    let storage = external.path().join("node-storage");
    write_config(dir.path(), &storage);

    let out = Command::new(env!("CARGO_BIN_EXE_lnpath"))
        .arg("--config")
        .arg(dir.path())
        .arg("a1b2c3d4e5f60718293a4b5c6d7e8f90")
        .output()
        .expect("lnpath runs");

    assert!(
        !dir.path().join("storage").exists(),
        "lnpath created {} although the config named another storage path (stderr: {})",
        dir.path().join("storage").display(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        storage.join("transport_identity").exists(),
        "lnpath did not use the configured storage_path (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn lnstatus_authenticates_against_a_daemon_with_a_configured_storage_path() {
    // The end-to-end shape of #241 as it was reported: daemon and client read
    // the same config, so they must derive the same storage directory and the
    // same RPC authkey (`SHA256(storage/transport_identity)`). When they did
    // not, the operator saw `shared-instance RPC error: authentication
    // failed` with nothing in either config to explain it.
    //
    // Abstract sockets are per network namespace, so the name is keyed by pid
    // to keep parallel runs apart.
    let name = format!("cfgstorage{}", std::process::id());
    let dir = tempfile::tempdir().expect("temp dir");
    let external = tempfile::tempdir().expect("temp dir");
    let storage = external.path().join("node-storage");
    write_shared_config(dir.path(), &storage, &name);

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lnsd"));
    cmd.arg("--config")
        .arg(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _reaper = Reaper(spawn_supervised(cmd).expect("lnsd starts"));

    assert!(
        wait_for_instance(&name, Duration::from_secs(10)),
        "daemon never bound the shared instance"
    );

    let out = Command::new(env!("CARGO_BIN_EXE_lnstatus"))
        .arg("--config")
        .arg(dir.path())
        .output()
        .expect("lnstatus runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "lnstatus could not query the daemon (stdout: {stdout}) (stderr: {stderr})"
    );
    assert!(
        !stderr.contains("authentication failed"),
        "lnstatus derived a different authkey than the daemon: {stderr}"
    );
}
