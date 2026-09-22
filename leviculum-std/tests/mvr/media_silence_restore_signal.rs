//! mvr for the daemon's two media verbs: a running `lnsd`, a real signal,
//! and a board that answers.
//!
//! [`leviculum_std::interfaces::FirmwareRequest::MediaSilence`] and
//! `MediaRestore` exist so a scenario can take a node off the air in the
//! middle of a run and put it back, while the daemon holds the board's data
//! port `TIOCEXCL` and nothing else on the host can reach it. That is a
//! statement about a process, a signal and a wire, so a unit test of the
//! match arm would prove none of it: the signal number has to be the one the
//! daemon installed a handler for, the frames have to leave the real port in
//! the real order, and the profile that comes back has to be the one that was
//! there before.
//!
//! The board here is a scripted stand-in on the far end of a `socat` pty
//! pair, not firmware: it accepts `TYPE_MEDIA_QUERY` and `TYPE_MEDIA_PROFILE`
//! and answers both with `TYPE_MEDIA_REPORT`, which is what
//! `leviculum-nrf/src/usb.rs` does with those two types on the data port.
//! What it cannot prove is that the nRF firmware's carriers actually go
//! quiet — that is a rig measurement and is named as untested in the report
//! this landed with.
//!
//! Its profile starts at **LoRa only**, deliberately. The absent-record
//! default is both carriers on (`MediaProfileWire::BOTH`), so a board that
//! started there would make "restored what was there" and "wrote the default"
//! the same two bytes, and the test would pass for a daemon that had
//! forgotten everything.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use leviculum_core::envelope::{
    self, decode_frame, encode_media_report, MediaProfileWire, MEDIA_FLAG_LORA,
};
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_std::interfaces::{FIRMWARE_MEDIA_RESTORE_SIGNAL, FIRMWARE_MEDIA_SILENCE_SIGNAL};
use leviculum_std::process::spawn_supervised;

/// Serial HW_MTU, the deframer bound the interface itself uses.
const HW_MTU: usize = 564;

/// How long the board is given to see a frame the daemon was told to send.
const FRAME_WAIT: Duration = Duration::from_secs(3);

/// How long a "nothing must reach the board" window is held open. Long
/// enough that a frame in flight would land inside it — the confirmed
/// frames below arrive in single-digit milliseconds — and short enough to
/// keep this file inside the mvr tier's budget.
const SILENCE_WINDOW: Duration = Duration::from_millis(600);

/// A real-time signal number no lnsd handler is installed for. Adjacent to
/// the two verbs on purpose: the risk worth controlling for is a handler
/// landing on the wrong number, and a wrong number is far likelier to be a
/// neighbour than something from the named range.
///
/// It cannot be the control *inside* a running scenario, which is what the
/// second test in this file is about: the default action for a real-time
/// signal is to terminate the process, so sending it proves 43 is unwired
/// by killing the daemon.
const UNLISTENED_RT_SIGNAL: i32 = 43;

/// The control that leaves the daemon running: SIGWINCH, which lnsd
/// installs no handler for and whose default action is to ignore it. A
/// terminal resize must not take a board off the air, and — because the
/// daemon survives it — the verbs that follow it in the same test are the
/// positive control that the board was reachable all along.
const IGNORED_SIGNAL: i32 = 28;

/// Resolve the release binary the integ runner and the mvr tier share.
///
/// Same resolution as `lncp_fetch_rust_responder`'s copy, and duplicated
/// rather than shared for the same reason that one is local: it is four
/// lines of path arithmetic, and a test-support module that two mvr files
/// import would have to live in the `rnsd_interop` harness, which the
/// Python suites load too.
fn release_bin(name: &str) -> PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let triple = std::env::var("CARGO_BUILD_TARGET")
        .unwrap_or_else(|_| "x86_64-unknown-linux-musl".to_string());
    let triple_release = target_dir.join(&triple).join("release").join(name);
    if triple_release.exists() {
        triple_release
    } else {
        target_dir.join("release").join(name)
    }
}

/// A linked pty pair from `socat`, the serial-cable stand-in the interop
/// suite uses (Codeberg #102). Dropping it tears both ends down.
struct PtyPair {
    process: Child,
    daemon_end: String,
    board_end: String,
}

impl PtyPair {
    fn spawn() -> PtyPair {
        let mut cmd = Command::new("socat");
        cmd.args(["-d", "-d", "pty,raw,echo=0", "pty,raw,echo=0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut process = spawn_supervised(cmd).expect("socat spawns (apt install socat)");
        let stderr = process.stderr.take().expect("socat stderr is piped");
        let mut ends = Vec::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.expect("socat stderr is readable");
            if let Some(idx) = line.find("/dev/pts/") {
                ends.push(
                    line[idx..]
                        .chars()
                        .take_while(|c| !c.is_whitespace())
                        .collect::<String>(),
                );
                if ends.len() == 2 {
                    break;
                }
            }
        }
        assert_eq!(ends.len(), 2, "socat must report two ptys");
        // socat needs a beat after logging the paths before the ptys are
        // wired; without it an open() races the link setup.
        std::thread::sleep(Duration::from_millis(200));
        PtyPair {
            process,
            daemon_end: ends[0].clone(),
            board_end: ends[1].clone(),
        }
    }
}

impl Drop for PtyPair {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// What the scripted board saw on its port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardFrame {
    /// `TYPE_MEDIA_QUERY`, empty payload.
    Query,
    /// `TYPE_MEDIA_PROFILE`, carrying this raw flag byte. Raw, not a
    /// decoded profile, so "wrote the both-on default" is visible as the
    /// byte it is.
    Profile(u8),
}

/// Open the board end and answer media frames on it until the port dies.
///
/// Returns the stream of frames it saw. The thread is detached: it ends
/// when `socat` goes away with the [`PtyPair`], which every exit path of
/// this file passes through.
fn spawn_board(port: &str, initial: MediaProfileWire) -> Receiver<BoardFrame> {
    spawn_board_inner(port, initial, true)
}

/// A board that takes the frames and says nothing back: firmware from
/// before the envelope, which drops a five-byte control frame in packet
/// parsing, and any board whose answer is lost.
fn spawn_deaf_board(port: &str) -> Receiver<BoardFrame> {
    spawn_board_inner(
        port,
        MediaProfileWire {
            lora_enabled: true,
            ble_enabled: false,
        },
        false,
    )
}

fn spawn_board_inner(port: &str, initial: MediaProfileWire, answers: bool) -> Receiver<BoardFrame> {
    let mut read_half = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(port)
        .expect("the board end of the pty opens");
    let mut write_half = read_half.try_clone().expect("the pty end clones");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut deframer = Deframer::with_max_frame(HW_MTU);
        let mut profile = initial;
        let mut buf = [0u8; 1024];
        let mut out = Vec::new();
        loop {
            let n = match read_half.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for r in deframer.process(&buf[..n]) {
                let DeframeResult::Frame(data) = r else {
                    continue;
                };
                let Ok(control) = decode_frame(&data) else {
                    continue;
                };
                let seen = match control.frame_type {
                    envelope::TYPE_MEDIA_QUERY => BoardFrame::Query,
                    envelope::TYPE_MEDIA_PROFILE => {
                        let flags = *control.payload.first().unwrap_or(&0xFF);
                        if let Some(p) = envelope::decode_media_profile_payload(control.payload) {
                            profile = p;
                        }
                        BoardFrame::Profile(flags)
                    }
                    _ => continue,
                };
                if tx.send(seen).is_err() {
                    return;
                }
                if !answers {
                    continue;
                }
                // The firmware answers both types with a media report
                // (`media_query_answer` / `media_profile_answer`), and both
                // carriers on this stand-in came up at boot, so running and
                // configured are the same value.
                frame(&encode_media_report(&profile, &profile), &mut out);
                if write_half.write_all(&out).is_err() || write_half.flush().is_err() {
                    return;
                }
                out.clear();
            }
        }
    });
    rx
}

/// A running `lnsd` whose only interface is a `SerialInterface` on `port`.
struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
}

impl Daemon {
    fn spawn(port: &str, instance: &str) -> Daemon {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("storage")).expect("storage dir");
        // No `frequency`: a plain serial pipe, so the interface pushes no
        // radio config at connect and the board's port stays quiet until a
        // signal says otherwise. `enable_transport = no` and
        // `respond_to_probes = no` for the same reason.
        let config = format!(
            "[reticulum]\n  \
               enable_transport = no\n  \
               share_instance = yes\n  \
               instance_name = {instance}\n  \
               respond_to_probes = no\n\n\
             [logging]\n  loglevel = 4\n\n\
             [interfaces]\n  \
               [[Board]]\n    \
                 type = SerialInterface\n    \
                 enabled = yes\n    \
                 port = {port}\n    \
                 speed = 115200\n"
        );
        fs::write(dir.path().join("config"), config).expect("config file");
        let lnsd = release_bin("lnsd");
        assert!(
            lnsd.exists(),
            "{} not found - run `just build-integ-bins` first",
            lnsd.display()
        );
        let mut cmd = Command::new(&lnsd);
        cmd.arg("--config")
            .arg(dir.path())
            .stdout(Stdio::from(
                fs::File::create(dir.path().join("stdout.log")).expect("stdout log"),
            ))
            .stderr(Stdio::from(
                fs::File::create(dir.path().join("stderr.log")).expect("stderr log"),
            ));
        let child = spawn_supervised(cmd).expect("lnsd spawns");
        Daemon { child, dir }
    }

    /// Everything the daemon has logged so far, both streams, because
    /// which one a tracing subscriber writes to is not this test's claim.
    fn logs(&self) -> String {
        let mut text = fs::read_to_string(self.dir.path().join("stdout.log")).unwrap_or_default();
        text.push_str(&fs::read_to_string(self.dir.path().join("stderr.log")).unwrap_or_default());
        text
    }

    /// Wait for a logged line containing `needle`, and return it.
    fn wait_for_log(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let logs = self.logs();
            if let Some(line) = logs.lines().find(|l| l.contains(needle)) {
                return line.to_string();
            }
            assert!(
                Instant::now() < deadline,
                "no log line containing {needle:?} within {timeout:?}; logs:\n{logs}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn signal(&self, number: i32) {
        let status = Command::new("kill")
            .arg(format!("-{number}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill -{number} failed: {status}");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The next frame the board saw, or a named failure.
fn expect_frame(rx: &Receiver<BoardFrame>, what: &str) -> BoardFrame {
    match rx.recv_timeout(FRAME_WAIT) {
        Ok(f) => f,
        Err(e) => panic!("the board saw no {what} within {FRAME_WAIT:?}: {e:?}"),
    }
}

/// Assert the board sees nothing for [`SILENCE_WINDOW`].
fn expect_no_frame(rx: &Receiver<BoardFrame>, why: &str) {
    match rx.recv_timeout(SILENCE_WINDOW) {
        Err(RecvTimeoutError::Timeout) => {}
        Ok(f) => panic!("{why}: the board saw {f:?}"),
        Err(RecvTimeoutError::Disconnected) => panic!("{why}: the board thread died"),
    }
}

#[test]
fn the_media_signals_silence_a_board_and_put_back_the_profile_it_had() {
    let pty = PtyPair::spawn();
    // LoRa only — see the module docs for why not the both-on default.
    let board_start = MediaProfileWire {
        lora_enabled: true,
        ble_enabled: false,
    };
    let board = spawn_board(&pty.board_end, board_start);
    let daemon = Daemon::spawn(&pty.daemon_end, "mvr-media-verbs");

    // The io task subscribes to the request channel when the port opens,
    // so a signal before this line reaches nothing.
    let online = daemon.wait_for_log("online on", Duration::from_secs(10));
    let iface = online
        .split_whitespace()
        .skip_while(|w| *w != "interface")
        .nth(1)
        .expect("the online line names the interface")
        .to_string();
    daemon.wait_for_log("Out-of-band control:", Duration::from_secs(10));

    // ---- A restore with no prior silence: the documented no-op. ----
    daemon.signal(FIRMWARE_MEDIA_RESTORE_SIGNAL);
    daemon.wait_for_log(
        &format!("MEDIA_RESTORE iface={iface} outcome=nothing-remembered"),
        FRAME_WAIT,
    );
    expect_no_frame(
        &board,
        "a restore with nothing remembered must write nothing at all",
    );

    // ---- The silence: query first, then both carriers clear. ----
    daemon.signal(FIRMWARE_MEDIA_SILENCE_SIGNAL);
    assert_eq!(
        expect_frame(&board, "media query"),
        BoardFrame::Query,
        "the daemon must read the board's profile before it overwrites it"
    );
    assert_eq!(
        expect_frame(&board, "silencing profile"),
        BoardFrame::Profile(0x00),
        "silence is both carrier flags clear"
    );
    // Read back off the board's own report, not off our own intent: the
    // daemon logs what the report said it is running.
    daemon.wait_for_log(
        &format!(
            "MEDIA_SILENCE iface={iface} outcome=applied remembered_lora=1 remembered_ble=0 \
             running_lora=0 running_ble=0 configured_lora=0 configured_ble=0"
        ),
        FRAME_WAIT,
    );

    // ---- Negative control, with the daemon still up and the board still
    // answering: a signal nobody listens to writes nothing. The restore
    // below is its positive control — same daemon, same port, same board
    // thread — so "nothing arrived" cannot be a dead wire. ----
    daemon.signal(IGNORED_SIGNAL);
    expect_no_frame(
        &board,
        "a signal no handler is installed for must reach no board",
    );

    // ---- The restore: the profile that was there, not the default. ----
    daemon.signal(FIRMWARE_MEDIA_RESTORE_SIGNAL);
    assert_eq!(
        expect_frame(&board, "restoring profile"),
        BoardFrame::Profile(MEDIA_FLAG_LORA),
        "the restore must write back the LoRa-only profile the board had, \
         not the both-on default (0x{:02x})",
        MediaProfileWire::BOTH.flags()
    );
    daemon.wait_for_log(
        &format!(
            "MEDIA_RESTORE iface={iface} outcome=restored restored_lora=1 restored_ble=0 \
             running_lora=1 running_ble=0 configured_lora=1 configured_ble=0"
        ),
        FRAME_WAIT,
    );

    // ---- And the memory is spent: a second restore is the no-op again,
    // which is what stops a restore from being a way to re-apply a
    // profile long after the silence it belonged to. ----
    daemon.signal(FIRMWARE_MEDIA_RESTORE_SIGNAL);
    expect_no_frame(&board, "a second restore has nothing left to restore");
    assert_eq!(
        daemon
            .logs()
            .matches(&format!(
                "MEDIA_RESTORE iface={iface} outcome=nothing-remembered"
            ))
            .count(),
        2,
        "both the restore before the silence and the one after it are no-ops"
    );
}

/// The negative control's other half, which cannot run in the test above
/// because it ends the daemon: [`UNLISTENED_RT_SIGNAL`] is not merely ignored,
/// it kills the process, because the default action for a real-time signal
/// is to terminate. That is the whole argument for writing the numbers out
/// rather than naming `SIGRTMIN` — a sender that resolves the name against
/// its own libc and misses by one does not get a no-op, it gets a dead
/// daemon. Termination by that very number is the proof that nothing in
/// lnsd listens for it: a process with a handler installed would have
/// survived and gone on running.
#[test]
fn an_unlistened_real_time_signal_is_not_a_media_verb() {
    let pty = PtyPair::spawn();
    let board = spawn_board(
        &pty.board_end,
        MediaProfileWire {
            lora_enabled: true,
            ble_enabled: false,
        },
    );
    let mut daemon = Daemon::spawn(&pty.daemon_end, "mvr-media-unlistened");
    daemon.wait_for_log("Out-of-band control:", Duration::from_secs(10));
    daemon.wait_for_log("online on", Duration::from_secs(10));

    daemon.signal(UNLISTENED_RT_SIGNAL);
    let status = daemon.child.wait().expect("the daemon is waitable");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(UNLISTENED_RT_SIGNAL),
        "an unhandled real-time signal terminates the process: {status}"
    );
    expect_no_frame(&board, "a signal nothing listens for writes no frame");
}

/// A board that does not report its profile is not silenced at all.
///
/// This is the one unrecoverable outcome the verb can produce: a board
/// whose carriers went off while the daemon never learned what they were
/// has nothing to be restored to, and the profile lives in the board's
/// flash, so the next boot comes up silent too. The daemon therefore
/// writes nothing when the query goes unanswered, and the restore that
/// follows has nothing remembered, which is how a scenario finds out.
///
/// The deaf board is not hypothetical: firmware from before the envelope
/// drops a five-byte control frame in packet parsing and answers nothing,
/// which is exactly the shape of a probe that times out.
#[test]
fn a_board_that_does_not_report_is_not_silenced() {
    let pty = PtyPair::spawn();
    let board = spawn_deaf_board(&pty.board_end);
    let daemon = Daemon::spawn(&pty.daemon_end, "mvr-media-deaf");
    let online = daemon.wait_for_log("online on", Duration::from_secs(10));
    let iface = online
        .split_whitespace()
        .skip_while(|w| *w != "interface")
        .nth(1)
        .expect("the online line names the interface")
        .to_string();

    daemon.signal(FIRMWARE_MEDIA_SILENCE_SIGNAL);
    assert_eq!(
        expect_frame(&board, "media query"),
        BoardFrame::Query,
        "the query is still asked; it is the answer that never comes"
    );
    // The daemon's own answer budget is 2 s, so this line is what ends
    // the wait, and by the time it is logged any profile frame would
    // already have been written.
    daemon.wait_for_log(
        &format!("MEDIA_SILENCE iface={iface} outcome=no-report"),
        Duration::from_secs(5),
    );
    expect_no_frame(
        &board,
        "a board whose profile could not be read must not be silenced",
    );

    // And nothing was remembered, so there is nothing to put back.
    daemon.signal(FIRMWARE_MEDIA_RESTORE_SIGNAL);
    daemon.wait_for_log(
        &format!("MEDIA_RESTORE iface={iface} outcome=nothing-remembered"),
        FRAME_WAIT,
    );
}
