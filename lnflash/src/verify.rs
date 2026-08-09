//! The **verify** axis: did the flash take?
//!
//! Two checks, because the weaker one alone has been wrong before.
//!
//! **Re-enumeration.** The application came back and the bootloader drive is
//! gone. Necessary, not sufficient: a board that re-enumerates is running
//! *something*, not necessarily what we just wrote.
//!
//! **The `[FW_BUILD]` banner.** Our firmware re-emits
//! `[FW_BUILD] git_sha=<sha> dirty=<bool>` on the debug port every few
//! seconds. Comparing that SHA against the one the manifest records is what
//! catches a silent touch-flash that did not actually take — board back,
//! old firmware still resident.
//!
//! One trap: **the debug CDC transmits only with DTR+RTS asserted.** Without
//! them a healthy board reads as silent, and the tool would report an
//! unverified flash on a board that is fine.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::sys::Fd;

/// A board running current firmware re-emits the banner every ~5 s, so one
/// window of this length catches it on the happy path without retrying.
pub const BANNER_WINDOW: Duration = Duration::from_secs(12);

/// A parsed `[FW_BUILD]` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FwBuild {
    pub git_sha: String,
    pub dirty: Option<bool>,
}

impl FwBuild {
    /// Whether this is the build the manifest expects.
    ///
    /// Compared by prefix, in whichever direction is shorter: the firmware
    /// emits a short SHA and a manifest may carry a full one. Both are the
    /// same commit, and refusing to match them would report every good
    /// flash as unverified.
    pub fn matches(&self, expected: &str) -> bool {
        let (a, b) = (
            self.git_sha.to_ascii_lowercase(),
            expected.trim().to_ascii_lowercase(),
        );
        if a.is_empty() || b.is_empty() {
            return false;
        }
        a.starts_with(&b) || b.starts_with(&a)
    }
}

/// Pull `[FW_BUILD]` out of a line of debug output. `None` for every other
/// line, of which there are many.
pub fn parse_fw_build(line: &str) -> Option<FwBuild> {
    let rest = line.split_once("[FW_BUILD]")?.1;
    let mut git_sha = None;
    let mut dirty = None;
    for field in rest.split_whitespace() {
        match field.split_once('=') {
            Some(("git_sha", value)) => git_sha = Some(value.trim().to_string()),
            Some(("dirty", value)) => dirty = value.trim().parse::<bool>().ok(),
            _ => {}
        }
    }
    let git_sha = git_sha.filter(|s| !s.is_empty())?;
    Some(FwBuild { git_sha, dirty })
}

/// The last `[FW_BUILD]` seen on `port` within `window`.
///
/// The *last*, not the first: the banner repeats, and a board that rebooted
/// mid-window should be judged on what it says now.
pub fn read_banner(port: &Path, window: Duration) -> io::Result<Option<FwBuild>> {
    let fd = Fd::open_serial(port)?;
    fd.set_debug_port()?;
    let raw = fd.read_until(Instant::now() + window, 1 << 20)?;
    Ok(last_banner(&String::from_utf8_lossy(&raw)))
}

/// The last `[FW_BUILD]` in a block of captured output.
pub fn last_banner(text: &str) -> Option<FwBuild> {
    text.lines().filter_map(parse_fw_build).next_back()
}

/// What the verify step concluded. Deliberately three-valued: "I could not
/// confirm" is not the same claim as "it is wrong", and a tool that
/// collapses them either cries wolf or hides a failed flash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Re-enumerated and the banner names the build we wrote.
    Confirmed { git_sha: String },
    /// Re-enumerated, but the banner names a different build. The flash did
    /// not take.
    WrongBuild { saw: String, expected: String },
    /// Re-enumerated, but no banner was read. Could be a board that needs
    /// longer, could be firmware that does not emit one.
    Unconfirmed { why: String },
    /// The application never came back.
    Absent,
}

impl Verdict {
    /// Only [`Verdict::Confirmed`] is success. Kept as a method so no call
    /// site can quietly decide that "unconfirmed" is good enough.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed { .. })
    }
}

/// Judge a banner against what the manifest says should be running.
pub fn judge(banner: Option<FwBuild>, expected: Option<&str>) -> Verdict {
    match (banner, expected) {
        (Some(seen), Some(want)) if seen.matches(want) => Verdict::Confirmed {
            git_sha: seen.git_sha,
        },
        (Some(seen), Some(want)) => Verdict::WrongBuild {
            saw: seen.git_sha,
            expected: want.to_string(),
        },
        (Some(seen), None) => Verdict::Unconfirmed {
            why: format!(
                "the board reports git_sha={}, but the manifest records no expected build to \
                 compare it against",
                seen.git_sha
            ),
        },
        (None, _) => Verdict::Unconfirmed {
            why: "no [FW_BUILD] banner on the debug port".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_our_firmware_emits_parses() {
        let banner = parse_fw_build("[FW_BUILD] git_sha=bb7c4f64 dirty=false").unwrap();
        assert_eq!(banner.git_sha, "bb7c4f64");
        assert_eq!(banner.dirty, Some(false));
    }

    #[test]
    fn a_banner_buried_in_other_output_still_parses() {
        let line = "0012345 INFO  [FW_BUILD] git_sha=bb7c4f64 dirty=true";
        assert_eq!(parse_fw_build(line).unwrap().dirty, Some(true));
    }

    #[test]
    fn ordinary_debug_lines_are_not_banners() {
        assert_eq!(parse_fw_build("PANIC_COUNT total=0"), None);
        assert_eq!(parse_fw_build(""), None);
        // A banner with no SHA says nothing and must not read as one.
        assert_eq!(parse_fw_build("[FW_BUILD] dirty=false"), None);
        assert_eq!(parse_fw_build("[FW_BUILD] git_sha= dirty=false"), None);
    }

    #[test]
    fn the_last_banner_wins_because_the_board_may_have_rebooted_mid_read() {
        let text = "[FW_BUILD] git_sha=aaaaaaaa dirty=false\n\
                    PANIC_COUNT total=0\n\
                    [FW_BUILD] git_sha=bb7c4f64 dirty=false\n";
        assert_eq!(last_banner(text).unwrap().git_sha, "bb7c4f64");
        assert_eq!(last_banner("nothing here\n"), None);
    }

    #[test]
    fn a_short_sha_and_a_full_one_are_the_same_commit() {
        let banner = parse_fw_build("[FW_BUILD] git_sha=bb7c4f64 dirty=false").unwrap();
        assert!(banner.matches("bb7c4f64"));
        assert!(banner.matches("bb7c4f6412345678901234567890123456789012"));
        assert!(banner.matches("BB7C4F64"));
        assert!(!banner.matches("cc7c4f64"));
        assert!(!banner.matches(""));
    }

    #[test]
    fn a_matching_banner_confirms_the_flash() {
        let banner = parse_fw_build("[FW_BUILD] git_sha=bb7c4f64 dirty=false");
        let verdict = judge(banner, Some("bb7c4f64"));
        assert_eq!(
            verdict,
            Verdict::Confirmed {
                git_sha: "bb7c4f64".into()
            }
        );
        assert!(verdict.is_confirmed());
    }

    #[test]
    fn a_board_still_running_the_old_firmware_is_caught() {
        // The silent touch-flash: the board came back, but it came back as
        // what it already was.
        let banner = parse_fw_build("[FW_BUILD] git_sha=deadbeef dirty=false");
        let verdict = judge(banner, Some("bb7c4f64"));
        assert!(matches!(verdict, Verdict::WrongBuild { .. }));
        assert!(!verdict.is_confirmed());
    }

    #[test]
    fn silence_is_unconfirmed_rather_than_confirmed_or_wrong() {
        let verdict = judge(None, Some("bb7c4f64"));
        assert!(matches!(verdict, Verdict::Unconfirmed { .. }));
        assert!(!verdict.is_confirmed());
        assert!(!matches!(verdict, Verdict::WrongBuild { .. }));
    }

    #[test]
    fn a_banner_with_nothing_to_compare_against_is_unconfirmed() {
        let banner = parse_fw_build("[FW_BUILD] git_sha=bb7c4f64 dirty=false");
        let verdict = judge(banner, None);
        assert!(!verdict.is_confirmed());
        assert!(format!("{verdict:?}").contains("bb7c4f64"));
    }

    #[test]
    fn an_absent_application_is_its_own_verdict() {
        assert!(!Verdict::Absent.is_confirmed());
    }
}
