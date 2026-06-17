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

/// Compile `source` against the cdylib and run it. Panics on any failure.
fn compile_and_run(source: &str, bin_name: &str) {
    let lib_dir = lib_dir();
    let so = lib_dir.join("libleviculum.so");
    if !so.exists() {
        eprintln!(
            "skipping: {} not found (build the glibc cdylib: `cargo test-ffi`)",
            so.display()
        );
        return;
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

    let run = Command::new(&out_bin)
        .env("LD_LIBRARY_PATH", &lib_dir)
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
