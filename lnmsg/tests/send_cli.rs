//! The command line as a user meets it: every exit code, through the real
//! binary.
//!
//! The unit tests in `src/` assert on the parsers and on the send loop against
//! a fake outbox. This asserts what a script sees — the exit code, and that
//! stdout stays clean of everything but a message id — because that contract
//! is the whole point of a non-interactive subcommand and no in-process test
//! can check the process's exit status.

use std::io::Write;
use std::process::{Command, Stdio};

const LNMSG: &str = env!("CARGO_BIN_EXE_lnmsg");

/// A well-formed address that nothing in these tests can reach.
const NOWHERE: &str = "aabbccddeeff00112233445566778899";

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `lnmsg` with an isolated state directory, feeding `stdin`.
///
/// The state directory matters: a shared one would make the tests order
/// dependent through the identity file, and would write into the developer's
/// real `~/.config/lnmsg`.
fn run(args: &[&str], stdin: &[u8]) -> Run {
    let home = tempfile::tempdir().expect("state dir");
    let mut command = Command::new(LNMSG);
    command
        .args(args)
        .env("LNMSG_HOME", home.path())
        // No inherited event-log file: these runs would append to whatever a
        // developer had set for something else.
        .env_remove("LEVICULUM_EVENT_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Supervised: a child that outlives a killed test run would hold the state
    // directory the tempdir is about to remove.
    let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn lnmsg");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(stdin)
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait for lnmsg");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn help_exits_zero_and_documents_the_send_subcommand() {
    let run = run(&["--help"], b"");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.stdout.contains("send"), "{}", run.stdout);
    assert!(
        run.stdout.contains("does not start a Reticulum stack"),
        "the help has to say a daemon is required: {}",
        run.stdout
    );
}

#[test]
fn a_malformed_address_is_a_usage_error() {
    let run = run(&["send", "not-an-address"], b"body\n");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stdout.is_empty(),
        "nothing may reach stdout when no message exists: {:?}",
        run.stdout
    );
    assert!(run.stderr.contains("hex"), "{}", run.stderr);
}

#[test]
fn a_short_address_is_a_usage_error_naming_the_length() {
    let run = run(&["send", "aabbccdd"], b"body\n");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("32 hex characters"), "{}", run.stderr);
}

#[test]
fn an_empty_body_is_a_usage_error() {
    let run = run(&["send", NOWHERE], b"");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(run.stdout.is_empty());
    assert!(run.stderr.contains("empty"), "{}", run.stderr);
}

#[test]
fn a_whitespace_only_body_is_an_empty_body() {
    let run = run(&["send", NOWHERE], b"   \n");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
}

/// The brief's rule: what is not built says so, rather than silently doing
/// something else. A script that asked for a mailbox and got a direct delivery
/// would believe an offline recipient had been reached.
#[test]
fn via_propagated_says_it_is_not_built_yet() {
    let run = run(&["send", NOWHERE, "--via", "propagated"], b"body\n");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(run.stdout.is_empty());
    assert!(
        run.stderr.contains("not built yet"),
        "the refusal must name itself: {}",
        run.stderr
    );
}

#[test]
fn an_unknown_via_value_is_rejected_by_the_parser() {
    let run = run(&["send", NOWHERE, "--via", "carrier-pigeon"], b"body\n");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
}

/// With no daemon there is nothing to queue into, and the message has to say
/// what is missing and how to fix it — never fall back to a private stack.
#[test]
fn with_no_daemon_running_the_error_names_the_daemon() {
    let instance = format!("lnmsg-nothing-here-{}", std::process::id());
    let run = run(&["send", NOWHERE, "--instance", &instance], b"body\n");
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(
        run.stdout.is_empty(),
        "no id may be printed when nothing was queued: {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains(&instance),
        "the error must name the instance it looked for: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("lnsd") && run.stderr.contains("rnsd"),
        "the error must say what to start: {}",
        run.stderr
    );
}

/// A corrupt identity file is the user's address. It must stop the program
/// rather than quietly become a different address.
#[test]
fn a_corrupt_identity_file_stops_the_run() {
    let home = tempfile::tempdir().expect("state dir");
    std::fs::write(home.path().join("identity"), b"not an identity").expect("write");

    let output = Command::new(LNMSG)
        .args(["send", NOWHERE, "hello"])
        .env("LNMSG_HOME", home.path())
        .env_remove("LEVICULUM_EVENT_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run lnmsg");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("address"),
        "the error must explain what the file is: {stderr}"
    );
    assert_eq!(
        std::fs::read(home.path().join("identity")).expect("read back"),
        b"not an identity",
        "the file must be left for the user to rescue"
    );
}

/// `LEVICULUM_EVENT_LOG` is the switch the concept asks for: start the program
/// with a log file and every transition is in it, in the documented format.
#[test]
fn the_event_log_records_the_run_in_the_documented_format() {
    let home = tempfile::tempdir().expect("state dir");
    let log = home.path().join("events.log");

    let output = Command::new(LNMSG)
        .args(["send", "nonsense"])
        .env("LNMSG_HOME", home.path())
        .env("LEVICULUM_EVENT_LOG", &log)
        .env("LEVICULUM_EVENT_NODE", "cli-test")
        .stdin(Stdio::null())
        .output()
        .expect("run lnmsg");
    assert_eq!(output.status.code(), Some(2));

    let text = std::fs::read_to_string(&log).expect("the event log file must exist");
    let line = text
        .lines()
        .find(|line| line.starts_with("LNMSG_DONE "))
        .unwrap_or_else(|| panic!("no LNMSG_DONE line in:\n{text}"));
    // EVENT_NAME first, node= second, t= last: the format's three fixed rules.
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields[0], "LNMSG_DONE");
    assert_eq!(fields[1], "node=cli-test");
    assert!(
        fields[fields.len() - 1].starts_with("t="),
        "the line must end with the relative timestamp: {line}"
    );
    assert!(line.contains("outcome=usage"), "{line}");
    assert!(line.contains("code=2"), "{line}");
    assert!(
        !text.contains("EVENT_SCHEMA_VIOLATION") && !text.contains("EVENT_FIELD_VIOLATION"),
        "the emitted events must satisfy their catalogue entries:\n{text}"
    );
}
