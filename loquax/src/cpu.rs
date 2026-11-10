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
//!
//! The sibling must be the same build (1525): every installed binary
//! resolves the same `lib/loquax/loquax-v3`, so an older `loquax-<sha>`
//! kept for a comparison or a rollback would otherwise run the newest v3
//! build and `--version` would name the other commit. Each build carries
//! its version string (the one `--version` prints, commit hash included)
//! in a marker frame in its read-only data; the starter finds the
//! sibling's `.rodata` through the ELF section header table, scans that
//! one section for the frame (no execution), and hands over only on an
//! equal id. On any other answer it runs the baseline and says so in one
//! line. (Not an ELF section of its own: `#[link_section]` is unsafe code,
//! which this crate forbids.)

use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
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

/// The build id marker's frame: the id sits between these in the binary,
/// so the starter finds it in a file by a scan, without running it. The
/// scan stops at each copy of the first byte; DEL is 30 times rarer in
/// the release `.rodata` than the 0x01 the frame began with first (4988
/// against 154856, 1525): the whole read of the 42 MB release binary went
/// from 1.90 to 1.67 ms, of which reading the 3.6 MB section is 1.0 ms.
macro_rules! mark_open {
    () => {
        "\x7floquax-build-id["
    };
}
macro_rules! mark_close {
    () => {
        "]\x7f"
    };
}

/// This build's id in its frame: the version string `--version` prints
/// without the CPU suffix (version and commit hash), so the baseline and
/// the v3 build of one commit agree. It sits in the read-only data of
/// every build. The needle the scan looks for is a slice of this same
/// static, so the binary holds the frame's opening exactly once, here.
static BUILD_MARK: &str = concat!(mark_open!(), env!("LEVICULUM_VERSION"), mark_close!());

/// The opening of the frame, as a slice of [`BUILD_MARK`].
fn mark_open() -> &'static [u8] {
    &BUILD_MARK.as_bytes()[..mark_open!().len()]
}

/// The most a build id may be.
const BUILD_ID_MAX: usize = 256;

/// The most read-only data the scan reads: the release build's `.rodata`
/// is 3.6 MB (1525), a file claiming more is not one of ours.
const RODATA_MAX: u64 = 64 << 20;

/// The most section headers the reader walks, a bound on a corrupt file.
const SECTIONS_MAX: u16 = 4096;

/// This build's id as the marker holds it: read through the static, so
/// the marker is referenced from code and the linker keeps it.
fn own_build_id() -> &'static str {
    let open = mark_open!().len();
    let close = mark_close!().len();
    &BUILD_MARK[open..BUILD_MARK.len() - close]
}

/// What the starter found at the v3 build's place.
#[derive(Debug, PartialEq, Eq)]
pub enum Sibling {
    /// No file there.
    Absent,
    /// A file whose build id is this, or `None` when it has none that
    /// could be read (not ELF, no section, unreadable).
    Present(Option<String>),
}

/// The short name of a build id in a log line: the first eight characters
/// of the commit hash inside the parentheses, or `unknown`.
pub fn short_id(id: Option<&str>) -> String {
    id.and_then(|id| {
        let start = id.rfind('(')? + 1;
        let end = start + id[start..].find(')')?;
        let hash = &id[start..end];
        Some(hash.chars().take(8).collect::<String>())
    })
    .filter(|hash| !hash.is_empty())
    .unwrap_or_else(|| "unknown".to_string())
}

/// The build id of the ELF file at `path`: the id in the first marker
/// frame inside its `.rodata`, found through the section header table
/// (the ELF header, the table, the section name table) and a scan of
/// that one section, a few megabytes. Only 64-bit little-endian ELF is
/// read, the only kind the starter runs on.
pub fn read_build_id(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let (offset, size) = rodata_span(&mut file)?;
    if size > RODATA_MAX {
        return None;
    }
    let mut rodata = vec![0u8; usize::try_from(size).ok()?];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut rodata).ok()?;
    find_build_id(&rodata)
}

/// The id in the first complete marker frame in `bytes`.
fn find_build_id(bytes: &[u8]) -> Option<String> {
    let open = mark_open();
    let close = mark_close!().as_bytes();
    let mut from = 0;
    while let Some(at) = find(&bytes[from..], open) {
        let start = from + at + open.len();
        let window = &bytes[start..bytes.len().min(start + BUILD_ID_MAX + close.len())];
        if let Some(end) = find(window, close) {
            if let Ok(id) = std::str::from_utf8(&window[..end]) {
                return Some(id.to_string());
            }
        }
        from = start;
    }
    None
}

/// The first position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, rest) = needle.split_first()?;
    let mut from = 0;
    while let Some(at) = haystack.get(from..)?.iter().position(|&b| b == first) {
        let start = from + at;
        if haystack.get(start + 1..start + needle.len()) == Some(rest) {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// The file offset and size of the `.rodata` section.
fn rodata_span(file: &mut File) -> Option<(u64, u64)> {
    let mut header = [0u8; 64];
    file.read_exact(&mut header).ok()?;
    // Magic, ELFCLASS64, ELFDATA2LSB.
    if header[..4] != *b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return None;
    }
    let shoff = u64_at(&header, 0x28)?;
    let shentsize = usize::from(u16_at(&header, 0x3A)?);
    let shnum = u16_at(&header, 0x3C)?;
    let shstrndx = usize::from(u16_at(&header, 0x3E)?);
    if shentsize < 0x40 || shnum == 0 || shnum > SECTIONS_MAX || shstrndx >= usize::from(shnum) {
        return None;
    }
    let mut table = vec![0u8; shentsize * usize::from(shnum)];
    file.seek(SeekFrom::Start(shoff)).ok()?;
    file.read_exact(&mut table).ok()?;
    let section = |index: usize| -> Option<(u32, u64, u64)> {
        let entry = table.get(index * shentsize..(index + 1) * shentsize)?;
        Some((
            u32_at(entry, 0)?,
            u64_at(entry, 0x18)?,
            u64_at(entry, 0x20)?,
        ))
    };
    let (_, names_offset, names_size) = section(shstrndx)?;
    if names_size > 1 << 20 {
        return None;
    }
    let mut names = vec![0u8; usize::try_from(names_size).ok()?];
    file.seek(SeekFrom::Start(names_offset)).ok()?;
    file.read_exact(&mut names).ok()?;
    let wanted = b".rodata\0";
    (0..usize::from(shnum)).find_map(|index| {
        let (name, offset, size) = section(index)?;
        names
            .get(usize::try_from(name).ok()?..)?
            .starts_with(wanted)
            .then_some((offset, size))
    })
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

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
/// binary's path, this build's id and what is at the v3 build's place.
pub fn decide(
    built_v3: bool,
    cpu_v3: bool,
    opt_out: Option<&OsStr>,
    exe: Option<&Path>,
    own_id: &str,
    sibling: impl Fn(&Path) -> Sibling,
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
    let Some(path) = exe.and_then(v3_path) else {
        return Start::Here;
    };
    match sibling(&path) {
        Sibling::Absent => Start::Here,
        Sibling::Present(Some(id)) if id == own_id => Start::Exec(path),
        Sibling::Present(id) => Start::HereSaying(format!(
            "CPU_START variant=baseline reason=sibling-build-mismatch sibling={}",
            short_id(id.as_deref())
        )),
    }
}

/// What is at `path` for the starter: nothing, or a file and its build id.
pub fn probe_sibling(path: &Path) -> Sibling {
    if path.is_file() {
        Sibling::Present(read_build_id(path))
    } else {
        Sibling::Absent
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
        own_build_id(),
        probe_sibling,
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

    const ID: &str = "0.7.0 (1a2b3c4d5e6f)";

    fn there(path: &Path) -> Sibling {
        if path == Path::new(V3) {
            Sibling::Present(Some(ID.to_string()))
        } else {
            Sibling::Absent
        }
    }

    #[test]
    fn a_v3_cpu_starts_the_v3_build_beside_the_package() {
        assert_eq!(
            decide(false, true, None, Some(Path::new(EXE)), ID, there),
            Start::Exec(PathBuf::from(V3))
        );
    }

    #[test]
    fn an_older_cpu_runs_the_baseline() {
        assert_eq!(
            decide(false, false, None, Some(Path::new(EXE)), ID, there),
            Start::Here
        );
    }

    #[test]
    fn the_v3_build_never_hands_over() {
        assert_eq!(
            decide(true, true, None, Some(Path::new(EXE)), ID, there),
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
                ID,
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
                ID,
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
            ID,
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
                ID,
                there
            ),
            Start::Here
        );
        assert_eq!(decide(false, true, None, None, ID, there), Start::Here);
        assert_eq!(
            decide(false, true, None, Some(Path::new("/loquax")), ID, there),
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

    #[test]
    fn a_sibling_of_another_build_keeps_the_baseline_and_says_so() {
        let other = |_: &Path| Sibling::Present(Some("0.6.9 (8adc925e0011)".to_string()));
        assert_eq!(
            decide(false, true, None, Some(Path::new(EXE)), ID, other),
            Start::HereSaying(
                "CPU_START variant=baseline reason=sibling-build-mismatch sibling=8adc925e"
                    .to_string()
            )
        );
        let unreadable = |_: &Path| Sibling::Present(None);
        assert_eq!(
            decide(false, true, None, Some(Path::new(EXE)), ID, unreadable),
            Start::HereSaying(
                "CPU_START variant=baseline reason=sibling-build-mismatch sibling=unknown"
                    .to_string()
            )
        );
    }

    /// Two fake siblings on the host (1525): a copy of this test binary,
    /// which carries this build's id, and a copy with the id's commit hash
    /// changed in place. Only the first is handed over to.
    #[test]
    fn the_starter_execs_only_a_sibling_of_its_own_build() {
        let own = std::env::current_exe().expect("test exe");
        let bytes = std::fs::read(&own).expect("read test exe");
        assert_eq!(read_build_id(&own).as_deref(), Some(own_build_id()));
        assert_eq!(own_build_id(), env!("LEVICULUM_VERSION"));

        // Beside the test binary, not in /tmp: each copy is the size of
        // this executable, tens of megabytes in a debug build.
        let dir = own
            .parent()
            .expect("exe dir")
            .join(format!("loquax-cpu-1525-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        let lib = dir.join("lib/loquax");
        std::fs::create_dir_all(&bin).expect("bin dir");
        std::fs::create_dir_all(&lib).expect("lib dir");
        let exe = bin.join("loquax");
        let sibling = lib.join("loquax-v3");

        // No sibling yet: the baseline, silently.
        assert_eq!(
            decide(false, true, None, Some(&exe), own_build_id(), probe_sibling),
            Start::Here
        );

        // Same build.
        std::fs::write(&sibling, &bytes).expect("write same");
        assert_eq!(
            decide(false, true, None, Some(&exe), own_build_id(), probe_sibling),
            Start::Exec(sibling.clone())
        );

        // Another build: the id in the section with a different hash.
        let other_id = {
            let open = own_build_id().rfind('(').expect("hash in id") + 1;
            let mut id = own_build_id().as_bytes().to_vec();
            for byte in &mut id[open..own_build_id().len() - 1] {
                *byte = if *byte == b'f' { b'e' } else { b'f' };
            }
            id
        };
        let mut patched = bytes.clone();
        let at = section_offset(&patched);
        patched[at..at + other_id.len()].copy_from_slice(&other_id);
        std::fs::write(&sibling, &patched).expect("write other");
        let other_text = String::from_utf8(other_id).expect("utf8");
        assert_eq!(
            read_build_id(&sibling).as_deref(),
            Some(other_text.as_str())
        );
        assert_eq!(
            decide(false, true, None, Some(&exe), own_build_id(), probe_sibling),
            Start::HereSaying(format!(
                "CPU_START variant=baseline reason=sibling-build-mismatch sibling={}",
                short_id(Some(&other_text))
            ))
        );

        // Not an ELF file at all.
        std::fs::write(&sibling, b"#!/bin/sh\n").expect("write script");
        assert_eq!(read_build_id(&sibling), None);
        assert!(matches!(
            decide(false, true, None, Some(&exe), own_build_id(), probe_sibling),
            Start::HereSaying(_)
        ));

        // The opt-out still wins over a matching sibling.
        std::fs::write(&sibling, &bytes).expect("write same again");
        assert_eq!(
            decide(
                false,
                true,
                Some(OsStr::new("baseline")),
                Some(&exe),
                own_build_id(),
                probe_sibling
            ),
            Start::Here
        );
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    /// The file offset of this build's id inside its marker frame.
    fn section_offset(bytes: &[u8]) -> usize {
        let open = mark_open();
        let mut from = 0;
        loop {
            let at = from + find(&bytes[from..], open).expect("marker in test exe");
            let start = at + open.len();
            if bytes[start..].starts_with(own_build_id().as_bytes()) {
                return start;
            }
            from = start;
        }
    }

    #[test]
    fn the_short_id_is_the_commit_hash() {
        assert_eq!(short_id(Some("0.7.0 (15fa07512345)")), "15fa0751");
        assert_eq!(short_id(Some("0.7.0 (unknown)")), "unknown");
        assert_eq!(short_id(Some("garbage")), "unknown");
        assert_eq!(short_id(None), "unknown");
    }
}
