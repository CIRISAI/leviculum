//! The starter (1469, #474): on an amd64 CPU with x86-64-v3 (AVX2, BMI2,
//! FMA and the rest of the level), the packaged `loquax` hands itself over
//! to `/usr/lib/loquax/loquax-v3`, the same program built with
//! `-C target-cpu=x86-64-v3`, before anything else runs.
//!
//! Why two binaries and not one with runtime dispatch: the window draws
//! through tiny-skia, whose wide types are chosen by
//! `cfg(target_feature)` at compile time, so a baseline build never
//! reaches its AVX2 paths whatever the CPU says. 1442 measured the v3
//! build at 1527x2099: the map's wheel 23 to 29 % faster, the real chats
//! 12 to 15 %, the synthetic chat 0 to 8 %, the map's drag the same (a
//! memcpy). The baseline binary stays for every other amd64 CPU.
//!
//! The decision is [`decide`], apart from the process so a test can ask
//! it. `LOQUAX_CPU=baseline` keeps the baseline build on a v3 CPU: for a
//! comparison, or for a pixel that differs (the man page, ENVIRONMENT).
//! The v3 binary itself never hands over: it is the one built with AVX2,
//! which [`BUILT_V3`] reads at compile time, so no loop is possible. A
//! build without the file beside it (a development build, the static
//! tarball) runs where it is.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The variable that keeps the baseline build on a v3 CPU.
pub const ENV: &str = "LOQUAX_CPU";

/// Where the package puts the v3 build, relative to the directory of the
/// running binary's directory: `/usr/bin/loquax` finds
/// `/usr/lib/loquax/loquax-v3`.
const V3_FROM_PREFIX: &str = "lib/loquax/loquax-v3";

/// This binary is the v3 build: AVX2 is a compile-time feature only of the
/// build with `-C target-cpu=x86-64-v3`.
pub const BUILT_V3: bool = cfg!(all(target_arch = "x86_64", target_feature = "avx2"));

/// What the starter does.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// Run this binary.
    Here,
    /// Run this binary, and say why on stderr first.
    HereSaying(String),
    /// Replace this process with the v3 build at this path.
    Exec(PathBuf),
}

/// The v3 build's place for a binary at `exe`.
pub fn v3_path(exe: &Path) -> Option<PathBuf> {
    Some(exe.parent()?.parent()?.join(V3_FROM_PREFIX))
}

/// The starter's decision from its inputs: whether this binary is the v3
/// build, whether the CPU runs v3, the value of [`ENV`], the running
/// binary's path and whether a file is there.
pub fn decide(
    built_v3: bool,
    cpu_v3: bool,
    opt_out: Option<&OsStr>,
    exe: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Start {
    if built_v3 {
        return Start::Here;
    }
    match opt_out {
        None => {}
        Some(value) if value == "baseline" => return Start::Here,
        Some(value) if value.is_empty() => {}
        Some(value) => {
            return Start::HereSaying(format!(
                "loquax: {ENV}={} is not `baseline`; ignored",
                value.to_string_lossy()
            ))
        }
    }
    if !cpu_v3 {
        return Start::Here;
    }
    match exe.and_then(v3_path) {
        Some(path) if exists(&path) => Start::Exec(path),
        _ => Start::Here,
    }
}

/// Whether this CPU runs code built for x86-64-v3: the features
/// `rustc --print cfg -C target-cpu=x86-64-v3` adds to the baseline, each
/// asked once (std caches the answers).
#[cfg(target_arch = "x86_64")]
pub fn cpu_has_v3() -> bool {
    std::arch::is_x86_feature_detected!("avx")
        && std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("bmi1")
        && std::arch::is_x86_feature_detected!("bmi2")
        && std::arch::is_x86_feature_detected!("fma")
        && std::arch::is_x86_feature_detected!("f16c")
        && std::arch::is_x86_feature_detected!("lzcnt")
        && std::arch::is_x86_feature_detected!("movbe")
        && std::arch::is_x86_feature_detected!("xsave")
        && std::arch::is_x86_feature_detected!("popcnt")
        && std::arch::is_x86_feature_detected!("sse4.2")
        && std::arch::is_x86_feature_detected!("cmpxchg16b")
}

#[cfg(not(target_arch = "x86_64"))]
pub fn cpu_has_v3() -> bool {
    false
}

/// The starter: returns when this binary is the one to run, and does not
/// return when the v3 build took the process over.
pub fn start() {
    if BUILT_V3 || !cfg!(target_arch = "x86_64") {
        return;
    }
    let exe = std::env::current_exe().ok();
    let opt_out = std::env::var_os(ENV);
    match decide(
        BUILT_V3,
        cpu_has_v3(),
        opt_out.as_deref(),
        exe.as_deref(),
        |path| path.is_file(),
    ) {
        Start::Here => {}
        Start::HereSaying(why) => eprintln!("{why}"),
        Start::Exec(path) => {
            use std::os::unix::process::CommandExt;
            let mut args = std::env::args_os();
            let mut command = std::process::Command::new(&path);
            if let Some(arg0) = args.next() {
                command.arg0(arg0);
            }
            // Only returns when the exec failed: the baseline goes on.
            let error = command.args(args).exec();
            eprintln!(
                "loquax: cannot start {}: {error}; running the baseline build",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/usr/bin/loquax";
    const V3: &str = "/usr/lib/loquax/loquax-v3";

    fn there(path: &Path) -> bool {
        path == Path::new(V3)
    }

    #[test]
    fn a_v3_cpu_starts_the_v3_build_beside_the_package() {
        assert_eq!(
            decide(false, true, None, Some(Path::new(EXE)), there),
            Start::Exec(PathBuf::from(V3))
        );
    }

    #[test]
    fn an_older_cpu_runs_the_baseline() {
        assert_eq!(
            decide(false, false, None, Some(Path::new(EXE)), there),
            Start::Here
        );
    }

    #[test]
    fn the_v3_build_never_hands_over() {
        assert_eq!(
            decide(true, true, None, Some(Path::new(EXE)), there),
            Start::Here
        );
    }

    #[test]
    fn baseline_in_the_environment_keeps_the_baseline() {
        assert_eq!(
            decide(
                false,
                true,
                Some(OsStr::new("baseline")),
                Some(Path::new(EXE)),
                there
            ),
            Start::Here
        );
        // Empty is unset.
        assert_eq!(
            decide(
                false,
                true,
                Some(OsStr::new("")),
                Some(Path::new(EXE)),
                there
            ),
            Start::Exec(PathBuf::from(V3))
        );
    }

    #[test]
    fn another_value_is_named_and_ignored() {
        match decide(
            false,
            true,
            Some(OsStr::new("v4")),
            Some(Path::new(EXE)),
            there,
        ) {
            Start::HereSaying(why) => assert!(why.contains("LOQUAX_CPU=v4"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_build_without_the_file_beside_it_runs_where_it_is() {
        assert_eq!(
            decide(
                false,
                true,
                None,
                Some(Path::new("/home/me/target/release/loquax")),
                there
            ),
            Start::Here
        );
        assert_eq!(decide(false, true, None, None, there), Start::Here);
        assert_eq!(
            decide(false, true, None, Some(Path::new("/loquax")), there),
            Start::Here
        );
    }

    #[test]
    fn this_test_binary_is_the_build_it_says() {
        // The v3 golden run (1469) builds the tests with
        // `-C target-cpu=x86-64-v3`; this pins that the flag reached them.
        let want = std::env::var("LOQUAX_EXPECT_V3").is_ok_and(|v| v == "1");
        if std::env::var_os("LOQUAX_EXPECT_V3").is_some() {
            assert_eq!(BUILT_V3, want);
        }
    }
}
