//! The **verify** axis: did the flash take?
//!
//! Two checks, because the weaker one alone has been wrong before.
//!
//! **Re-enumeration.** The bootloader left the bus and the application came
//! back. Necessary, not sufficient: a board that re-enumerates is running
//! *something*, not necessarily what we just wrote. A board that never left
//! the bus never rebooted at all, and nothing it says afterwards is about
//! the image we wrote — that case is `Absent`, not a build claim.
//!
//! **The `[FW_BUILD]` banner.** Our firmware re-emits
//! `[FW_BUILD] git_sha=<sha> dirty=<bool>` on the debug port every five
//! seconds (`leviculum-nrf/src/bin/t114.rs:999`,
//! `leviculum-nrf/src/bin/rak4631.rs:1047`). Comparing that SHA against the
//! one the manifest records is what catches a silent touch-flash that did
//! not actually take — board back, old firmware still resident.
//!
//! **The banner has to be one the board emitted after our reset** (Codeberg
//! #378). Reading a window and keeping the last line in it accepted
//! whatever happened to be in the port's input queue, which is a queue
//! filled before the question was asked; on a two-board run that named the
//! sha the board had been running *before* the flash and reported a good
//! flash as a failed one. [`fresh_banner`] flushes that queue first and then
//! waits for a line to arrive, so the answer is about the firmware running
//! now or there is no answer at all.
//!
//! **A build claim carries its provenance** ([`Source`]). A verdict that
//! names a sha and nothing else cannot be checked: the #378 recurrence of
//! 2026-09-11 printed `the board reports git_sha=b9b4a9c3` and settling
//! whether that line had come from this board's own port at all took two
//! capture files, a hand correlation, and stayed undecided. Every claim now
//! states which mechanism read it, on which path, behind which node, and how
//! long after the flush the line arrived.
//!
//! One trap: **the debug CDC transmits only with DTR+RTS asserted.** Without
//! them a healthy board reads as silent, and the tool would report an
//! unverified flash on a board that is fine.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::sys::Fd;

/// How long to wait for a `[FW_BUILD]` line the board emitted *after* the
/// reset this tool triggered.
///
/// Three banner periods. The firmware emits one every 5 s, so a healthy
/// board answers in the first period and the happy path pays no more than
/// that; the budget is larger because two periods can be lost without the
/// board being at fault. The port is opened when the application device
/// appears on the bus, which is before the banner task's first tick (it
/// sleeps 5 s before its first line, `leviculum-nrf/src/bin/t114.rs:1000`),
/// and the line that would have landed in that first period can be the one
/// the flush cut in half. A board that has still said nothing after three
/// periods is not slow, it is silent — which is `unknown`, and never a
/// claim about which build is running.
pub const FRESH_BANNER_BUDGET: Duration = Duration::from_secs(15);

/// How long one read step waits before looking again. Short enough that a
/// port that went away is noticed promptly, long enough not to spin — the
/// same interval `watch::POLL` uses on the same port.
const READ_STEP: Duration = Duration::from_millis(200);

/// A board writing without newlines must not grow the line buffer without
/// bound. Past this the buffer is dropped: it holds no complete line, so it
/// holds no answer.
const CARRY_MAX: usize = 16 * 1024;

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

/// The first `[FW_BUILD]` the board emits **after** this call, or `None` if
/// it says nothing before `deadline`.
///
/// Three rules, and each of them is a way the old "last banner in a window"
/// read was wrong (#378):
///
/// 1. **The input queue is flushed first.** Everything in it was said
///    before we asked, so it cannot answer a question about the image we
///    just wrote. One `tcflush`, the same one every control transaction
///    does ([`Fd::drain_input`]) and for the same reason.
/// 2. **Only a complete line counts.** A line is accepted when its newline
///    has been read, never from the bytes in hand: a half-written
///    `git_sha=daa8b8e` reads as `git_sha=daa8` and would be reported as a
///    *different* build — a false failure manufactured out of a partial
///    read. What survives a tear is the tail of a line, and a tail that
///    still carries the `[FW_BUILD]` token carries everything after it
///    intact, so a torn line is either unparseable or right.
/// 3. **The first answer wins, and there is no second window.** The board
///    repeats the banner; the first one that arrives after the flush is by
///    construction from the session running now, and waiting longer to
///    prefer a later one only invites a reset mid-wait to be read as an
///    answer.
///
/// A port that goes away mid-wait ends the read with `None`: the caller
/// still holds the budget and can reopen. Silence is `None`, and `None` is
/// "unknown" — it is never turned into a build.
pub fn fresh_banner(fd: &Fd, deadline: Instant) -> io::Result<Option<FwBuild>> {
    fd.drain_input()?;
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let Some(bytes) = fd.read_available(READ_STEP.min(remaining))? else {
            // EOF: this fd is bound to a driver instance that is gone.
            return Ok(None);
        };
        carry.extend_from_slice(&bytes);
        while let Some(pos) = carry.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = carry.drain(..=pos).collect();
            if let Some(build) = parse_fw_build(&String::from_utf8_lossy(&line)) {
                return Ok(Some(build));
            }
        }
        if carry.len() > CARRY_MAX {
            carry.clear();
        }
    }
}

/// Where a build claim came from.
///
/// Codeberg #378 asks the confirmation to say which mechanism decided it,
/// and the 2026-09-11 recurrence says why: the tool printed a sha and
/// nothing else, so settling "did it read its own board's port?" needed two
/// capture files, a hand correlation, and still ended undecided. A claim
/// that carries the path it was read on, the node behind that path, and how
/// long after the flush the line arrived is decidable from the tool's own
/// output. The delay is the load-bearing number: a line that arrives in
/// milliseconds is something that was already in flight, a banner from a
/// board that has just booted arrives seconds in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The path that was opened, as the bus resolved it: the board's
    /// `by-id` link where udev has one, the bare node otherwise.
    pub port: PathBuf,
    /// The device node behind that path, which the fd was proven against
    /// before a byte was read (`flow::open_debug`).
    pub node: PathBuf,
    /// How long after the port's input queue was flushed the line arrived.
    pub after: Duration,
}

impl Source {
    /// One clause naming the mechanism and the port, for the sentence a
    /// verdict prints. Names the node separately only when the path opened
    /// was not already the node.
    pub fn describe(&self) -> String {
        let where_ = if self.port == self.node {
            format!("{}", self.node.display())
        } else {
            format!("{} ({})", self.port.display(), self.node.display())
        };
        format!(
            "read as a [FW_BUILD] banner line on {where_}, {:.1} s after that port was flushed",
            self.after.as_secs_f32()
        )
    }
}

/// A build claim and its provenance, which travel together because a sha
/// without a source is a claim nobody can check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub build: FwBuild,
    pub source: Source,
}

/// What the verify step concluded. Deliberately three-valued: "I could not
/// confirm" is not the same claim as "it is wrong", and a tool that
/// collapses them either cries wolf or hides a failed flash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Re-enumerated and the board's first line after the reset names the
    /// build we wrote.
    Confirmed { git_sha: String, source: Source },
    /// Re-enumerated, and the board's first line after the reset names a
    /// different build. The flash did not take.
    WrongBuild {
        saw: String,
        expected: String,
        source: Source,
    },
    /// Re-enumerated, but which build is running could not be established.
    /// Could be a board that needs longer, could be firmware that emits no
    /// banner, could be a debug port that never appeared.
    Unconfirmed { why: String },
    /// The application never came back, or the board never rebooted at all.
    Absent,
}

impl Verdict {
    /// Only [`Verdict::Confirmed`] is success. Kept as a method so no call
    /// site can quietly decide that "unconfirmed" is good enough.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed { .. })
    }

    /// Whether this verdict is a *contradiction* — the board said something
    /// that rules the flash out — rather than an absence of evidence.
    ///
    /// The distinction is the whole point of the three values, and it is
    /// what the process exit code is built on: a wrong build or a board
    /// that never came back needs the flash done again, an unconfirmed one
    /// needs the *read* done again.
    pub fn contradicts(&self) -> bool {
        matches!(self, Self::WrongBuild { .. } | Self::Absent)
    }

    /// One sentence for the closing summary, where a `Debug` rendering of
    /// an `Option<Verdict>` used to print `Some(WrongBuild { saw: … })` at
    /// an operator.
    pub fn describe(&self) -> String {
        match self {
            Self::Confirmed { git_sha, source } => {
                format!("running git_sha={git_sha} — {}", source.describe())
            }
            Self::WrongBuild {
                saw,
                expected,
                source,
            } => {
                format!(
                    "the write did not take — the board reports git_sha={saw}, not {expected} — {}",
                    source.describe()
                )
            }
            Self::Unconfirmed { why } => {
                format!("flashed, and the running build is unknown — {why}")
            }
            Self::Absent => "the application never came back".to_string(),
        }
    }
}

/// Judge what the board said against what the manifest says should be
/// running.
///
/// `seen` is a `Result` rather than an `Option` so that a caller with
/// nothing to show has to say why it has nothing: that sentence is the
/// whole content of an `unknown`, and an empty one sends the operator to
/// the board instead of to the port.
pub fn judge(seen: Result<Reading, String>, expected: Option<&str>) -> Verdict {
    match (seen, expected) {
        (Ok(seen), Some(want)) if seen.build.matches(want) => Verdict::Confirmed {
            git_sha: seen.build.git_sha,
            source: seen.source,
        },
        (Ok(seen), Some(want)) => Verdict::WrongBuild {
            saw: seen.build.git_sha,
            expected: want.to_string(),
            source: seen.source,
        },
        (Ok(seen), None) => Verdict::Unconfirmed {
            why: format!(
                "the board reports git_sha={}, but the manifest records no expected build to \
                 compare it against ({})",
                seen.build.git_sha,
                seen.source.describe()
            ),
        },
        (Err(why), _) => Verdict::Unconfirmed { why },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::testpty::Pty;
    use std::thread;

    /// The sha the board was running before the flash, and the one the
    /// bundle carries — the two from Codeberg #378.
    const OLD: &str = "ead0bce";
    const NEW: &str = "daa8b8e";

    fn banner(sha: &str) -> String {
        format!("[FW_BUILD] git_sha={sha} dirty=false\r\n")
    }

    /// A board's if00, as the bus resolves it on the rig: the by-id link and
    /// the node behind it.
    const BY_ID: &str = "/dev/serial/by-id/usb-leviculum_RAK4631_DEC9947DAD9D2869-if00";
    const NODE: &str = "/dev/ttyACM3";

    fn source() -> Source {
        Source {
            port: PathBuf::from(BY_ID),
            node: PathBuf::from(NODE),
            after: Duration::from_millis(2400),
        }
    }

    /// What a read off a board's debug port amounts to: the build, and where
    /// it was read.
    fn read(sha: &str) -> Reading {
        Reading {
            build: parse_fw_build(&banner(sha)).unwrap(),
            source: source(),
        }
    }

    /// Long enough that a scheduler hiccup cannot fail a test, short
    /// enough that a broken read still returns while somebody is watching.
    const TEST_BUDGET: Duration = Duration::from_secs(5);

    /// A stub board's debug port, with DTR+RTS raised the way the real one
    /// is opened.
    fn debug_port(pty: &Pty) -> Fd {
        let fd = Fd::open_serial(&pty.slave_path).expect("opening the pty slave");
        fd.set_debug_port().expect("raising DTR and RTS");
        fd
    }

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
        let verdict = judge(Ok(read("bb7c4f64")), Some("bb7c4f64"));
        assert_eq!(
            verdict,
            Verdict::Confirmed {
                git_sha: "bb7c4f64".into(),
                source: source(),
            }
        );
        assert!(verdict.is_confirmed());
        assert!(!verdict.contradicts());
    }

    #[test]
    fn a_board_still_running_the_old_firmware_is_caught() {
        // The silent touch-flash: the board came back, but it came back as
        // what it already was. This is a contradiction, not an absence —
        // the board said so itself, after the reset.
        let verdict = judge(Ok(read("deadbeef")), Some("bb7c4f64"));
        assert!(matches!(verdict, Verdict::WrongBuild { .. }));
        assert!(!verdict.is_confirmed());
        assert!(verdict.contradicts());
    }

    #[test]
    fn silence_is_unconfirmed_rather_than_confirmed_or_wrong() {
        let verdict = judge(Err("said nothing".into()), Some("bb7c4f64"));
        assert!(matches!(verdict, Verdict::Unconfirmed { .. }));
        assert!(!verdict.is_confirmed());
        // The load-bearing clause of #378: an undecidable confirmation must
        // not be reported as a failed flash, and must name no sha at all.
        assert!(!verdict.contradicts());
        assert!(!matches!(verdict, Verdict::WrongBuild { .. }));
        assert!(
            verdict.describe().contains("unknown"),
            "{}",
            verdict.describe()
        );
    }

    #[test]
    fn a_banner_with_nothing_to_compare_against_is_unconfirmed() {
        let verdict = judge(Ok(read("bb7c4f64")), None);
        assert!(!verdict.is_confirmed());
        assert!(format!("{verdict:?}").contains("bb7c4f64"));
        // Even the verdict that decides nothing says where it read the sha
        // it quotes.
        assert!(verdict.describe().contains(BY_ID), "{}", verdict.describe());
    }

    #[test]
    fn a_confirmation_says_which_mechanism_decided_it_and_on_which_port() {
        // The clause of #378 that the first fix left open: the tool has to
        // say which of the two reads decided the confirmation. A line naming
        // only a sha is a claim the operator cannot check.
        let line = judge(Ok(read("bb7c4f64")), Some("bb7c4f64")).describe();
        assert!(line.contains("[FW_BUILD] banner line"), "{line}");
        assert!(line.contains(BY_ID), "{line}");
        assert!(line.contains(NODE), "{line}");
        assert!(line.contains("2.4 s"), "{line}");
    }

    #[test]
    fn a_wrong_build_names_the_port_it_read_the_sha_on() {
        // #378 as it recurred on 2026-09-11: `the board reports
        // git_sha=b9b4a9c3, not de6e74ed` named no port and no timing, so
        // deciding whether that line had come from this board at all needed
        // two capture files and ended undecided. The failure line now
        // carries both.
        let line = judge(Ok(read("b9b4a9c3")), Some("de6e74ed")).describe();
        assert!(
            line.contains("b9b4a9c3") && line.contains("de6e74ed"),
            "{line}"
        );
        assert!(line.contains(BY_ID) && line.contains(NODE), "{line}");
        assert!(line.contains("2.4 s"), "{line}");
    }

    #[test]
    fn a_line_that_arrived_at_once_says_so() {
        // The tell the delay exists for: a banner from a board that has just
        // booted arrives seconds in, so a sha delivered in the first
        // milliseconds after the flush was already in flight and the claim
        // deserves the doubt the number puts on it.
        let instant = Source {
            after: Duration::ZERO,
            ..source()
        };
        assert!(
            instant.describe().contains("0.0 s"),
            "{}",
            instant.describe()
        );
    }

    #[test]
    fn a_port_with_no_by_id_link_is_named_once_rather_than_twice() {
        let bare = Source {
            port: PathBuf::from(NODE),
            node: PathBuf::from(NODE),
            after: Duration::from_millis(500),
        };
        let text = bare.describe();
        assert_eq!(text.matches(NODE).count(), 1, "{text}");
    }

    #[test]
    fn an_absent_application_is_its_own_verdict() {
        assert!(!Verdict::Absent.is_confirmed());
        // A board that never came back needs the flash done again, so it
        // belongs on the same side of the exit code as a wrong build.
        assert!(Verdict::Absent.contradicts());
    }

    #[test]
    fn the_build_line_from_before_the_reset_is_never_the_answer() {
        // Codeberg #378, as it happened: the port carries the sha the board
        // was running BEFORE the flash, and the new firmware only speaks up
        // afterwards. The old line is in the queue before the read starts,
        // which is exactly what made the tool name it.
        let pty = Pty::open();
        pty.write_raw(banner(OLD).as_bytes());
        let fd = debug_port(&pty);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(200));
                pty.write_raw(banner(NEW).as_bytes());
            });
            let seen = fresh_banner(&fd, Instant::now() + TEST_BUDGET)
                .expect("reading the stub board")
                .expect("the board spoke after the reset");
            assert_eq!(seen.git_sha, NEW);
            assert!(judge(
                Ok(Reading {
                    build: seen,
                    source: source()
                }),
                Some(NEW)
            )
            .is_confirmed());
        });
    }

    #[test]
    fn a_board_that_only_spoke_before_the_reset_is_unknown_not_the_old_sha() {
        // The other half of #378: nothing arrives after the flush. The old
        // line must not be reported as what is running now — under the old
        // rule this run printed `WrongBuild { saw: "ead0bce" }` and exited
        // non-zero on a board that was fine.
        let pty = Pty::open();
        pty.write_raw(banner(OLD).as_bytes());
        let fd = debug_port(&pty);
        let seen = fresh_banner(&fd, Instant::now() + Duration::from_millis(600))
            .expect("reading the stub board");
        assert_eq!(seen, None, "a line from the previous life is not an answer");

        let verdict = judge(
            seen.map(|build| Reading {
                build,
                source: source(),
            })
            .ok_or_else(|| "no [FW_BUILD] line arrived".to_string()),
            Some(NEW),
        );
        assert!(
            matches!(verdict, Verdict::Unconfirmed { .. }),
            "{verdict:?}"
        );
        assert!(!verdict.describe().contains(OLD), "{}", verdict.describe());
    }

    #[test]
    fn a_board_that_keeps_saying_the_old_sha_after_the_reset_is_still_caught() {
        // The freshness rule must not blunt the check it protects: a board
        // that answers, after the flush, with the build it was already
        // running is a flash that did not take.
        let pty = Pty::open();
        let fd = debug_port(&pty);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(200));
                pty.write_raw(banner(OLD).as_bytes());
            });
            let seen = fresh_banner(&fd, Instant::now() + TEST_BUDGET)
                .expect("reading the stub board")
                .expect("the board spoke");
            assert_eq!(
                judge(
                    Ok(Reading {
                        build: seen,
                        source: source()
                    }),
                    Some(NEW)
                ),
                Verdict::WrongBuild {
                    saw: OLD.into(),
                    expected: NEW.into(),
                    source: source(),
                }
            );
        });
    }

    #[test]
    fn a_half_written_line_is_not_a_different_build() {
        // A partial read of `git_sha=daa8b8e` would parse as `daa8` and be
        // reported as some other build — a failure manufactured out of a
        // truncated line. Only the newline makes a line an answer.
        let pty = Pty::open();
        let fd = debug_port(&pty);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(150));
                pty.write_raw(b"[FW_BUILD] git_sha=daa8");
                thread::sleep(Duration::from_millis(300));
                pty.write_raw(b"b8e dirty=false\r\n");
            });
            let seen = fresh_banner(&fd, Instant::now() + TEST_BUDGET)
                .expect("reading the stub board")
                .expect("the line completed");
            assert_eq!(seen.git_sha, NEW);
        });
    }

    #[test]
    fn other_debug_output_does_not_end_the_wait() {
        // The board says plenty that is not a banner; the read waits for
        // the banner rather than concluding on the first line it sees.
        let pty = Pty::open();
        let fd = debug_port(&pty);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                pty.write_raw(b"[LORA] RX 183 bytes rssi=-69\r\nPANIC_COUNT total=0\r\n");
                thread::sleep(Duration::from_millis(200));
                pty.write_raw(banner(NEW).as_bytes());
            });
            let seen = fresh_banner(&fd, Instant::now() + TEST_BUDGET)
                .expect("reading the stub board")
                .expect("the banner came after the chatter");
            assert_eq!(seen.git_sha, NEW);
        });
    }

    #[test]
    fn a_silent_port_costs_the_budget_and_no_more() {
        let pty = Pty::open();
        let fd = debug_port(&pty);
        let started = Instant::now();
        assert_eq!(
            fresh_banner(&fd, Instant::now() + Duration::from_millis(400)).unwrap(),
            None
        );
        assert!(started.elapsed() >= Duration::from_millis(400));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
