//! Compiles and runs the C example programs against the real cdylib.
//!
//! This is the per-phase acceptance test: a C program that links
//! `libleviculum.so` and exercises the public API end to end. It must be run
//! against the glibc target that produces a shippable `.so`:
//!
//! ```sh
//! cargo test-ffi            # alias: -p leviculum-ffi --target x86_64-unknown-linux-gnu
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

/// A TCP port on loopback the kernel has just confirmed free: bind `:0`, read
/// the number, drop the listener.
///
/// This is the tree's existing idiom — `tests/support/mod.rs:165` in this same
/// crate, `lblogd/tests/web_plain.rs:24`,
/// `leviculum-lxmf-node/tests/two_node_loopback.rs:84` — and the point of
/// Codeberg #206 is that the C examples were the one family that did not use
/// it. It is a narrow race (the consumer binds a moment after the probe
/// releases), which #206 rules out as the mechanism it saw and names as the
/// next suspect should the failure recur.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("bind an ephemeral loopback port")
        .port()
}

/// Compile `source` against the cdylib and run it with `args`. Panics on any
/// failure.
fn compile_and_run_args(source: &str, bin_name: &str, args: &[String]) {
    let Some(out_bin) = compile(source, bin_name) else {
        return;
    };
    let run = Command::new(&out_bin)
        .args(args)
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

/// Compile `source` against the cdylib and run it. Panics on any failure.
fn compile_and_run(source: &str, bin_name: &str) {
    compile_and_run_args(source, bin_name, &[]);
}

/// Compile `source` and run it with a freshly allocated loopback address as
/// `argv[1]`.
///
/// The two-node C examples used to carry a literal `127.0.0.1:4587x`, which
/// sits inside this host's `ip_local_port_range` (32768-60999): any concurrent
/// `bind("127.0.0.1:0")` in the workspace could be handed exactly that number,
/// and while it held it the example's server could not bind, its announce
/// timed out, and every check downstream of a working node failed with it
/// (Codeberg #206).
fn compile_and_run_on_free_addr(source: &str, bin_name: &str) {
    let addr = format!("127.0.0.1:{}", free_port());
    compile_and_run_args(source, bin_name, &[addr]);
}

#[test]
fn c_phase_a_acceptance() {
    compile_and_run("examples/c/phase_a.c", "phase_a_c");
}

#[test]
fn c_phase_b_acceptance() {
    compile_and_run_on_free_addr("examples/c/phase_b.c", "phase_b_c");
}

#[test]
fn c_phase_c_acceptance() {
    compile_and_run_on_free_addr("examples/c/phase_c.c", "phase_c_c");
}

#[test]
fn c_phase_d_acceptance() {
    compile_and_run_on_free_addr("examples/c/phase_d.c", "phase_d_c");
}

#[test]
fn c_phase_e_acceptance() {
    compile_and_run_on_free_addr("examples/c/phase_e.c", "phase_e_c");
}

#[test]
fn c_daemon_acceptance() {
    compile_and_run_args(
        "examples/c/daemon.c",
        "daemon_c",
        &[free_port().to_string()],
    );
}

/// Standing canary for Codeberg #206
/// (`docs/src/concepts/checks-and-citations.md` §Standing canaries).
///
/// The fix for #206 is a *negative* property — the two-node examples carry no
/// default address — and nothing about a green `c_phase_d_acceptance` proves
/// it. Re-adding `const char *addr = "127.0.0.1:45874";` as a fallback would
/// leave every test above passing while quietly restoring the bug, because the
/// harness would still pass a good port and the program would still ignore the
/// literal until the day it did not.
///
/// So: run each of them with no argument and require the usage exit. This dies
/// the moment a default comes back.
#[test]
fn the_two_node_examples_refuse_to_run_without_an_address() {
    for (source, bin) in [
        ("examples/c/phase_b.c", "phase_b_c_noargs"),
        ("examples/c/phase_c.c", "phase_c_c_noargs"),
        ("examples/c/phase_d.c", "phase_d_c_noargs"),
        ("examples/c/phase_e.c", "phase_e_c_noargs"),
        ("examples/c/daemon.c", "daemon_c_noargs"),
    ] {
        let Some(out_bin) = compile(source, bin) else {
            return;
        };
        let run = Command::new(&out_bin)
            .env("LD_LIBRARY_PATH", lib_dir())
            .output()
            .expect("failed to run compiled C test");
        assert_eq!(
            run.status.code(),
            Some(2),
            "{source} must refuse to run without an address rather than fall back \
             to a literal port (Codeberg #206); it exited {:?}",
            run.status.code()
        );
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            stderr.contains("usage:"),
            "{source} must say what it wants: {stderr}"
        );
    }
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
/// same binary periculum mounts for a node whose adapter is `c-lnsd`.
#[test]
fn c_lnsd_runs_as_daemon() {
    let Some(bin) = compile("examples/c/lnsd.c", "c_lnsd_test") else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let port = free_port();
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
    let port = free_port();
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
    let port = free_port();
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

/// Poll `path` for up to `secs` for a `destination: <hex>` line and return the
/// hex. The listener prints it to stderr once it is up and announcing.
fn read_dest(path: &Path, secs: u64) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(line) = s.lines().find_map(|l| l.strip_prefix("destination: ")) {
                return Some(line.trim().to_string());
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// End-to-end test of the `levcat` pipe: a listener and a connector, two
/// separate C processes, pipe a line of text over a real link. Exercises the
/// tutorial's core pattern (poll on stdin plus the event fd, link send/receive)
/// from a standalone program. The connector reads the line on stdin, sends it,
/// hits EOF and closes; the listener writes it to stdout and exits.
#[test]
fn c_levcat_pipes_a_line_end_to_end() {
    let Some(bin) = compile("examples/c/levcat.c", "levcat_c") else {
        return;
    };
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");

    let dir = tempfile::tempdir().expect("tempdir");
    let store_l = dir.path().join("listen");
    let store_c = dir.path().join("connect");
    std::fs::create_dir_all(&store_l).unwrap();
    std::fs::create_dir_all(&store_c).unwrap();
    let listen_out = dir.path().join("listen.out");
    let listen_err = dir.path().join("listen.err");
    let in_path = dir.path().join("input.txt");
    let payload = "hello over the mesh\n";
    std::fs::write(&in_path, payload).unwrap();

    // The listener: no stdin, data to a file, status (the destination) to a file.
    let mut listen = Command::new(&bin)
        .args(["listen", store_l.to_str().unwrap(), &addr])
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&listen_out).unwrap())
        .stderr(std::fs::File::create(&listen_err).unwrap())
        .env("LD_LIBRARY_PATH", lib_dir())
        .spawn()
        .expect("spawn listen");

    let dest = read_dest(&listen_err, 15).expect("listener never printed its destination");

    // The connector reads the payload from a file on stdin; at EOF it closes.
    let mut connect = Command::new(&bin)
        .args(["connect", store_c.to_str().unwrap(), &addr, &dest])
        .stdin(std::fs::File::open(&in_path).unwrap())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("LD_LIBRARY_PATH", lib_dir())
        .spawn()
        .expect("spawn connect");

    let connect_status = wait_timeout(&mut connect, 30).expect("connector did not finish in time");
    assert!(
        connect_status.success(),
        "connector exited with {:?}",
        connect_status.code()
    );
    let listen_status = wait_timeout(&mut listen, 20).expect("listener did not finish in time");
    assert!(
        listen_status.success(),
        "listener exited with {:?}",
        listen_status.code()
    );

    let got = std::fs::read_to_string(&listen_out).expect("listener stdout");
    assert!(
        got.contains("hello over the mesh"),
        "listener stdout did not carry the piped line: {got:?}"
    );
}
