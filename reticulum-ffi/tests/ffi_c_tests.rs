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
