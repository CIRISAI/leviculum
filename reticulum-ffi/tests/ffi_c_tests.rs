//! Compiles and runs the C example programs against the real cdylib.
//!
//! This is the per-phase acceptance test: a C program that links
//! `libleviculum.so` and exercises the public API end to end. It must be run
//! against the glibc target that produces a shippable `.so`:
//!
//! ```sh
//! cargo test-ffi            # alias: -p reticulum-ffi --target x86_64-unknown-linux-gnu
//! ```
//!
//! Under the workspace musl default no `.so` is produced (cdylib is
//! unsupported there), so the test skips with a clear message instead of
//! failing.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory holding the built `libleviculum.so`.
///
/// The test executable runs from `<target>/<profile>/deps/`, so the cdylib is
/// one directory up. This is triple- and profile-agnostic.
fn lib_dir() -> PathBuf {
    let exe = env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("deps parent")
        .to_path_buf()
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

/// Compile `source` against the cdylib, returning the binary path. Returns
/// `None` when the `.so` is absent (musl default), so callers skip cleanly.
/// Panics on a compile failure.
fn compile(source: &str, bin_name: &str) -> Option<PathBuf> {
    let lib_dir = lib_dir();
    let so = lib_dir.join("libleviculum.so");
    if !so.exists() {
        eprintln!(
            "skipping: {} not found (build the glibc cdylib: `cargo test-ffi`)",
            so.display()
        );
        return None;
    }

    // The cdylib carries SONAME libleviculum.so.0 (set in build.rs), so the
    // runtime loader looks for that name. Cargo names the build output
    // libleviculum.so, so provide the SONAME symlink next to it.
    let soname = lib_dir.join("libleviculum.so.0");
    if !soname.exists() {
        let _ = std::os::unix::fs::symlink("libleviculum.so", &soname);
    }

    let crate_dir = crate_dir();
    let source = crate_dir.join(source);
    // The generated header lives at the crate root; the example includes
    // "leviculum.h".
    let header_root = crate_dir.clone();
    let out_bin = lib_dir.join(bin_name);

    let status = Command::new("cc")
        .arg(&source)
        .arg("-o")
        .arg(&out_bin)
        .arg(format!("-I{}", header_root.display()))
        .arg(format!("-L{}", lib_dir.display()))
        .arg("-lleviculum")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .status()
        .expect("failed to invoke cc");
    assert!(
        status.success(),
        "cc failed to compile {}",
        source.display()
    );
    Some(out_bin)
}

/// Compile `source` against the cdylib and run it. Panics on any failure.
fn compile_and_run(source: &str, bin_name: &str) {
    let Some(out_bin) = compile(source, bin_name) else {
        return;
    };
    let run = Command::new(&out_bin)
        .env("LD_LIBRARY_PATH", lib_dir())
        .status()
        .expect("failed to run compiled C test");
    assert!(
        run.success(),
        "C test {} exited with {:?}",
        bin_name,
        run.code()
    );
}

#[test]
fn c_phase_a_acceptance() {
    compile_and_run("examples/c/phase_a.c", "phase_a_c");
}

#[test]
fn c_phase_b_acceptance() {
    compile_and_run("examples/c/phase_b.c", "phase_b_c");
}

#[test]
fn c_phase_c_acceptance() {
    compile_and_run("examples/c/phase_c.c", "phase_c_c");
}

#[test]
fn c_phase_d_acceptance() {
    compile_and_run("examples/c/phase_d.c", "phase_d_c");
}

#[test]
fn c_phase_e_acceptance() {
    compile_and_run("examples/c/phase_e.c", "phase_e_c");
}

#[test]
fn c_daemon_acceptance() {
    compile_and_run("examples/c/daemon.c", "daemon_c");
}

#[test]
fn c_radio_acceptance() {
    compile_and_run("examples/c/radio.c", "radio_c");
}

#[test]
fn c_crypto_acceptance() {
    compile_and_run("examples/c/crypto.c", "crypto_c");
}

#[test]
fn c_ratchet_acceptance() {
    compile_and_run("examples/c/ratchet.c", "ratchet_c");
}

#[test]
fn c_proof_acceptance() {
    compile_and_run("examples/c/proof.c", "proof_c");
}

#[test]
fn c_stats_acceptance() {
    compile_and_run("examples/c/stats.c", "stats_c");
}

/// The `lnsd.c` example is a real daemon, not a self-terminating acceptance
/// program: it loads a config, comes up, and runs until signalled. Spawn it,
/// confirm it stays up, then SIGTERM it and require a clean exit. This is the
/// same binary used as the `c-api` node in the reticulum-integ mesh.
#[test]
fn c_lnsd_runs_as_daemon() {
    let Some(bin) = compile("examples/c/lnsd.c", "c_lnsd_test") else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let name = format!("clnsd-test-{port}");
    let cfg = format!(
        "[reticulum]\n  enable_transport = no\n  share_instance = yes\n  \
         instance_name = {name}\n\n[interfaces]\n  [[Test TCP Server]]\n    \
         type = TCPServerInterface\n    enabled = yes\n    listen_ip = 127.0.0.1\n    \
         listen_port = {port}\n    mode = gateway\n"
    );
    std::fs::write(dir.path().join("config"), cfg).expect("write config");

    let mut child = Command::new(&bin)
        .arg("--config")
        .arg(dir.path())
        .env("LD_LIBRARY_PATH", lib_dir())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn c-lnsd");

    // It must load the config, bind the TCP server, open the shared instance,
    // and stay up.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "c-lnsd exited before it was signalled"
    );

    // SIGTERM triggers the signal handler and an orderly stop/free.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = child.wait().expect("wait c-lnsd");
    assert!(
        status.success(),
        "c-lnsd exited with {:?} after SIGTERM",
        status.code()
    );
}

/// Wait up to `secs` for a child to exit; on timeout, kill it and return None.
fn wait_timeout(child: &mut std::process::Child, secs: u64) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(st) = child.try_wait().expect("try_wait") {
            return Some(st);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// End-to-end test of the `lncp` file-copy tool: a receiver and a sender, two
/// separate C processes, copy a file over a real link with a resource transfer.
/// The strongest "the API lets a C developer build real tools" check, since it
/// exercises the whole stack (identity, announce, path, link, resource) from a
/// standalone program, not a test harness.
#[test]
fn c_lncp_copies_a_file_end_to_end() {
    let Some(bin) = compile("examples/c/lncp.c", "lncp_c") else {
        return;
    };
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let addr = format!("127.0.0.1:{port}");

    let dir = tempfile::tempdir().expect("tempdir");
    let store_r = dir.path().join("recv");
    let store_s = dir.path().join("send");
    std::fs::create_dir_all(&store_r).unwrap();
    std::fs::create_dir_all(&store_s).unwrap();
    let in_path = dir.path().join("input.bin");
    let out_path = dir.path().join("output.bin");
    // A 64 KiB payload that needs a real multi-part resource transfer.
    let content: Vec<u8> = (0..65536u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(&in_path, &content).unwrap();

    let mut recv = Command::new(&bin)
        .args([
            "recv",
            store_r.to_str().unwrap(),
            &addr,
            out_path.to_str().unwrap(),
        ])
        .env("LD_LIBRARY_PATH", lib_dir())
        .spawn()
        .expect("spawn recv");
    // Give the receiver a moment to bind and start announcing.
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let mut send = Command::new(&bin)
        .args([
            "send",
            store_s.to_str().unwrap(),
            &addr,
            in_path.to_str().unwrap(),
        ])
        .env("LD_LIBRARY_PATH", lib_dir())
        .spawn()
        .expect("spawn send");

    let send_status = wait_timeout(&mut send, 45).expect("sender did not finish in time");
    assert!(
        send_status.success(),
        "sender exited with {:?}",
        send_status.code()
    );
    let recv_status = wait_timeout(&mut recv, 20).expect("receiver did not finish in time");
    assert!(
        recv_status.success(),
        "receiver exited with {:?}",
        recv_status.code()
    );

    let copied = std::fs::read(&out_path).expect("output file written");
    assert_eq!(copied, content, "the copied file must match the original");
}

/// The same `lncp` tool, but the two clients attach to a running `c-lnsd` over
/// its shared-instance IPC socket (the way `rncp`/`rnx` attach to a daemon)
/// instead of bringing up their own interfaces. Proves the file-copy data path
/// works through the daemon, which relays between its two local clients.
#[test]
fn c_lncp_copies_via_shared_instance() {
    let Some(lncp) = compile("examples/c/lncp.c", "lncp_shared_c") else {
        return;
    };
    let Some(lnsd) = compile("examples/c/lnsd.c", "lncp_shared_lnsd_c") else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let instance = format!("lncp-ipc-{port}");

    // The daemon: shares an instance, no interfaces of its own needed since the
    // two clients are local.
    let dconf = dir.path().join("daemon");
    std::fs::create_dir_all(&dconf).unwrap();
    std::fs::write(
        dconf.join("config"),
        format!(
            "[reticulum]\n  enable_transport = yes\n  share_instance = yes\n  \
             instance_name = {instance}\n"
        ),
    )
    .unwrap();

    let in_path = dir.path().join("input.bin");
    let out_path = dir.path().join("output.bin");
    let content: Vec<u8> = (0..40000u32)
        .map(|i| (i.wrapping_mul(40503) >> 11) as u8)
        .collect();
    std::fs::write(&in_path, &content).unwrap();
    let store_r = dir.path().join("recv");
    let store_s = dir.path().join("send");
    std::fs::create_dir_all(&store_r).unwrap();
    std::fs::create_dir_all(&store_s).unwrap();

    let mut daemon = Command::new(&lnsd)
        .arg("--config")
        .arg(&dconf)
        .env("LD_LIBRARY_PATH", lib_dir())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn c-lnsd");
    // Let the daemon bind its IPC socket before the clients attach.
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let mut recv = Command::new(&lncp)
        .args([
            "recv-shared",
            store_r.to_str().unwrap(),
            &instance,
            out_path.to_str().unwrap(),
        ])
        .env("LD_LIBRARY_PATH", lib_dir())
        .spawn()
        .expect("spawn recv-shared");
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let mut send = Command::new(&lncp)
        .args([
            "send-shared",
            store_s.to_str().unwrap(),
            &instance,
            in_path.to_str().unwrap(),
        ])
        .env("LD_LIBRARY_PATH", lib_dir())
        .spawn()
        .expect("spawn send-shared");

    let send_status = wait_timeout(&mut send, 45).expect("sender did not finish in time");
    let recv_status = wait_timeout(&mut recv, 20).expect("receiver did not finish in time");
    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(
        send_status.success(),
        "sender exited with {:?}",
        send_status.code()
    );
    assert!(
        recv_status.success(),
        "receiver exited with {:?}",
        recv_status.code()
    );
    let copied = std::fs::read(&out_path).expect("output file written");
    assert_eq!(copied, content, "the copied file must match the original");
}
