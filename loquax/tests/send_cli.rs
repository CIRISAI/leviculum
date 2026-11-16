//! The command line as a user meets it: every exit code, through the real
//! binary.
//!
//! The unit tests in `src/` assert on the parsers and on the send loop against
//! a fake outbox. This asserts what a script sees — the exit code, and that
//! stdout stays empty — because that contract is the whole point of a
//! non-interactive subcommand and no in-process test can check the process's
//! exit status. Since 2026-08-21 stdout is empty on success too; that half
//! needs a daemon to reach and lives in `python_interop.rs`.

use std::io::Write;
use std::process::{Command, Stdio};

const LNMSG: &str = env!("CARGO_BIN_EXE_loquax");

/// A well-formed address that nothing in these tests can reach.
const NOWHERE: &str = "aabbccddeeff00112233445566778899";

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `loquax` with an isolated state directory, feeding `stdin`.
///
/// The state directory matters: a shared one would make the tests order
/// dependent through the identity file, and would write into the developer's
/// real `~/.config/loquax`.
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
    let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
    // A run whose arguments are rejected exits before it ever reads stdin, so
    // this write races the child's exit and legitimately loses under load with
    // EPIPE. Losing that race IS the behaviour under test, not a failure: the
    // body was never going to be consumed. Every assertion in these tests reads
    // the exit code and stderr, neither of which depends on the write landing,
    // so a broken pipe is dropped and any other write error still panics.
    // Without this the suite went red only when the machine was busy -- which
    // is to say, only in a full workspace run.
    match child.stdin.take().expect("stdin is piped").write_all(stdin) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(e) => panic!("write stdin: {e}"),
    }
    let output = child.wait_with_output().expect("wait for loquax");
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

/// Plain `loquax` is the messenger (Lew, 2026-10-26), in the window where
/// there is a display (2026-11-02, 1342), and its help says so first, says
/// how it picks and how to insist, then lists the commands for scripts.
#[test]
fn bare_help_names_the_messenger_first_then_the_subcommands() {
    let run = run(&["--help"], b"");
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.stdout.lines().next(),
        Some(
            "the Leviculum messenger; with no arguments it opens the window, or the terminal \
             view where there is no display"
        ),
        "{}",
        run.stdout
    );
    for words in [
        "WAYLAND_DISPLAY or DISPLAY",
        "`loquax gui` insists on the window",
        "`loquax tui` insists on the terminal view",
    ] {
        assert!(
            run.stdout.contains(words),
            "{words} missing: {}",
            run.stdout
        );
    }
    for command in ["send", "fetch", "address"] {
        assert!(
            run.stdout.contains(&format!("  {command} ")),
            "{command} missing: {}",
            run.stdout
        );
    }
}

/// Without the `tui` feature a plain `loquax` has no messenger to open: one
/// line on stderr and the usage exit code, never a silent success.
#[cfg(not(feature = "tui"))]
#[test]
fn bare_lnmsg_without_the_tui_feature_is_a_usage_error() {
    let run = run(&[], b"");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(run.stdout.is_empty(), "stdout: {}", run.stdout);
    assert_eq!(run.stderr.lines().count(), 1, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("`tui` feature"), "{}", run.stderr);
}

/// With it, the bare form takes the messenger's options: a bad `--size`
/// is rejected by the messenger's own check, exactly as under `loquax tui`.
#[cfg(feature = "tui")]
#[test]
fn bare_lnmsg_takes_the_messenger_options() {
    for args in [
        &["--script", "/nonexistent", "--size", "3x3"][..],
        &["tui", "--script", "/nonexistent", "--size", "3x3"][..],
    ] {
        let run = run(args, b"");
        assert_eq!(run.code, Some(2), "{args:?} stderr: {}", run.stderr);
        assert!(
            run.stderr.contains("--size 3x3: want WxH"),
            "{args:?}: {}",
            run.stderr
        );
    }
}

/// The messenger's options and a subcommand do not mix: `--to` before
/// `send` is an argument error, not a send with a stray flag.
#[cfg(feature = "tui")]
#[test]
fn messenger_options_before_a_subcommand_are_a_usage_error() {
    let run = run(&["--to", NOWHERE, "send", NOWHERE, "hi"], b"");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("cannot be used with"), "{}", run.stderr);
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

/// `--via propagated` is a real path now; with no daemon it fails exactly the
/// way a direct send does, an operational failure naming the daemon — never
/// a silent direct delivery, and never exit 0 or 3 for a message that went
/// nowhere.
#[test]
fn via_propagated_without_a_daemon_is_an_operational_failure() {
    let instance = format!("loquax-nothing-here-{}", std::process::id());
    let run = run(
        &[
            "send",
            NOWHERE,
            "--via",
            "propagated",
            "--instance",
            &instance,
            "--verbose",
        ],
        b"body\n",
    );
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(run.stdout.is_empty());
    assert!(
        run.stderr.contains(&instance),
        "the failure must name the missing daemon: {}",
        run.stderr
    );
}

/// A malformed --pn is caught before the body is read or a daemon dialled,
/// like every other argument error.
#[test]
fn a_malformed_pn_is_a_usage_error() {
    let run = run(&["send", NOWHERE, "--pn", "zz"], b"body\n");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("--pn"), "{}", run.stderr);
}

/// `loquax address` needs no daemon: it mints the identity on first use and
/// prints the same 32-hex address every run after.
#[test]
fn address_prints_a_stable_32_hex_address_without_a_daemon() {
    let home = tempfile::tempdir().expect("state dir");
    let output = |()| {
        let mut command = Command::new(LNMSG);
        command
            .arg("address")
            .env("LNMSG_HOME", home.path())
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    };
    let first = output(());
    assert_eq!(first.status.code(), Some(0));
    let address = String::from_utf8_lossy(&first.stdout).trim().to_string();
    assert_eq!(address.len(), 32, "one 32-hex line: {address:?}");
    assert!(address.bytes().all(|b| b.is_ascii_hexdigit()));

    let second = output(());
    assert_eq!(
        String::from_utf8_lossy(&second.stdout).trim(),
        address,
        "the address is the persistent identity's, not a fresh mint per run"
    );
}

#[test]
fn an_unknown_via_value_is_rejected_by_the_parser() {
    let run = run(&["send", NOWHERE, "--via", "carrier-pigeon"], b"body\n");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
}

/// With no daemon there is nothing to queue into, and the message has to say
/// what is missing and how to fix it — never fall back to a private stack.
/// The instance and the daemons are the `--verbose` text since 1265.
#[test]
fn with_no_daemon_running_the_error_names_the_daemon() {
    let instance = format!("loquax-nothing-here-{}", std::process::id());
    let run = run(
        &["send", NOWHERE, "--instance", &instance, "--verbose"],
        b"body\n",
    );
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

/// An empty `--from` is refused rather than silently replaced by the account
/// name: an operator who set the name explicitly has to hear that the value
/// cannot be used, not discover weeks later that the announces went out under
/// something else.
#[test]
fn an_empty_from_is_a_usage_error() {
    let run = run(&["send", NOWHERE, "--from", "", "body"], b"");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(run.stdout.is_empty());
    assert!(
        run.stderr.contains("--from") && run.stderr.contains("indistinguishable"),
        "the error must name the flag and say why: {}",
        run.stderr
    );
}

#[test]
fn a_whitespace_only_from_is_the_same_error() {
    let run = run(&["send", NOWHERE, "--from", "   ", "body"], b"");
    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
}

/// An empty `LNMSG_DISPLAY_NAME` is refused for the same reason. The env var
/// exists for the cron case, which is exactly where a silent fallback would go
/// unnoticed longest.
#[test]
fn an_empty_display_name_variable_is_a_usage_error() {
    let home = tempfile::tempdir().expect("state dir");
    let output = Command::new(LNMSG)
        .args(["send", NOWHERE, "body"])
        .env("LNMSG_HOME", home.path())
        .env("LNMSG_DISPLAY_NAME", "")
        .env_remove("LEVICULUM_EVENT_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run loquax");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LNMSG_DISPLAY_NAME"), "{stderr}");
}

/// Read the `from=` and `source=` fields of the run's `LNMSG_SENDER` line.
fn sender_line(log: &std::path::Path) -> (String, String) {
    let text = std::fs::read_to_string(log).expect("the event log file must exist");
    let line = text
        .lines()
        .find(|line| line.starts_with("LNMSG_SENDER "))
        .unwrap_or_else(|| panic!("no LNMSG_SENDER line in:\n{text}"));
    let field = |key: &str| {
        line.split_whitespace()
            .find_map(|token| token.strip_prefix(key))
            .unwrap_or_else(|| panic!("no {key} field in {line}"))
            .to_string()
    };
    assert!(
        !text.contains("EVENT_SCHEMA_VIOLATION") && !text.contains("EVENT_FIELD_VIOLATION"),
        "the emitted events must satisfy their catalogue entries:\n{text}"
    );
    (field("from="), field("source="))
}

/// The cron case, at the level a cron job actually meets it: **no environment
/// at all**. `$USER` and `$LOGNAME` are gone, and the name still has to be the
/// account's — resolved from the password database — rather than the tool's.
///
/// `env_clear` is `env -i`: the child gets only what is set after it. The run
/// itself fails (there is no daemon), which is irrelevant here — the name is
/// resolved and logged before anything can be attached to.
#[test]
fn with_no_environment_at_all_the_name_still_comes_from_the_password_database() {
    let Some(expected) = leviculum_std::user::passwd_name() else {
        // No passwd entry for this uid: the fallback chain is what is left,
        // and it is covered by the unit tests. Nothing to prove here.
        return;
    };
    let home = tempfile::tempdir().expect("state dir");
    let log = home.path().join("events.log");

    let output = Command::new(LNMSG)
        .args([
            "send",
            NOWHERE,
            "body",
            "--instance",
            "loquax-no-such-daemon",
        ])
        .env_clear()
        // Only the two paths the program cannot invent: where its identity
        // lives, and where to write the log this test reads. Neither carries a
        // user name.
        .env("LNMSG_HOME", home.path())
        .env("LEVICULUM_EVENT_LOG", &log)
        .stdin(Stdio::null())
        .output()
        .expect("run loquax");
    assert_eq!(
        output.status.code(),
        Some(1),
        "with no daemon this fails after resolving the name: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let (from, source) = sender_line(&log);
    assert_eq!(
        from, expected,
        "an empty environment must not change who we are"
    );
    assert_eq!(
        source, "passwd",
        "with USER and LOGNAME gone, only the password database can have answered"
    );
    assert_ne!(from, "Loquax", "the tool's name is not the sender's name");
}

/// `--from` beats `LNMSG_DISPLAY_NAME`, through the real binary.
#[test]
fn the_flag_beats_the_environment_variable_in_the_shipped_binary() {
    let home = tempfile::tempdir().expect("state dir");
    let log = home.path().join("events.log");

    let output = Command::new(LNMSG)
        .args(["send", NOWHERE, "body", "--from", "hamster"])
        // A daemon may well be running on a developer's host; naming one that
        // is not keeps the run short and its exit code predictable.
        .args(["--instance", "loquax-no-such-daemon"])
        .env("LNMSG_HOME", home.path())
        .env("LNMSG_DISPLAY_NAME", "from-the-env")
        .env("LEVICULUM_EVENT_LOG", &log)
        .stdin(Stdio::null())
        .output()
        .expect("run loquax");
    assert_eq!(output.status.code(), Some(1));

    assert_eq!(
        sender_line(&log),
        ("hamster".to_string(), "flag".to_string())
    );
}

/// …and the variable beats the resolved account name.
#[test]
fn the_environment_variable_beats_the_account_name_in_the_shipped_binary() {
    let home = tempfile::tempdir().expect("state dir");
    let log = home.path().join("events.log");

    let output = Command::new(LNMSG)
        .args(["send", NOWHERE, "body"])
        .args(["--instance", "loquax-no-such-daemon"])
        .env("LNMSG_HOME", home.path())
        .env("LNMSG_DISPLAY_NAME", "lew_at_schneckenschreck")
        .env("LEVICULUM_EVENT_LOG", &log)
        .stdin(Stdio::null())
        .output()
        .expect("run loquax");
    assert_eq!(output.status.code(), Some(1));

    assert_eq!(
        sender_line(&log),
        ("lew_at_schneckenschreck".to_string(), "env".to_string())
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
        .expect("run loquax");

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
        .expect("run loquax");
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

/// `loquax address` under `umask 0002`, in a fresh state directory.
#[cfg(unix)]
fn address_under_umask_0002(home: &std::path::Path) -> std::process::Output {
    Command::new("sh")
        .args(["-c", "umask 0002 && exec \"$0\" address", LNMSG])
        .env("LNMSG_HOME", home)
        .env_remove("LEVICULUM_EVENT_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run loquax")
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// The identity is the private key: 0600 whatever the umask. Under 0002 it
/// came out 0664, readable by the whole group (found 2026-10-29, 1119).
#[cfg(unix)]
#[test]
fn a_minted_identity_is_owner_only_under_a_group_umask() {
    let home = tempfile::tempdir().expect("state dir");
    let output = address_under_umask_0002(home.path());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(mode(&home.path().join("identity")), 0o600);
}

/// A key an older loquax left group-readable is narrowed when it is loaded,
/// and stays the same key: the address does not change.
#[cfg(unix)]
#[test]
fn a_group_readable_identity_is_narrowed_at_load() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().expect("state dir");
    let first = address_under_umask_0002(home.path());
    assert!(first.status.success(), "{first:?}");
    let path = home.path().join("identity");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    let key = std::fs::read(&path).expect("read");

    let second = address_under_umask_0002(home.path());
    assert!(second.status.success(), "{second:?}");
    assert_eq!(mode(&path), 0o600);
    assert_eq!(std::fs::read(&path).expect("read"), key, "the same key");
    assert_eq!(first.stdout, second.stdout, "the same address");
}

/// 1119: `--config` goes before the subcommand as well as after it, for every
/// subcommand. `loquax --config X home join CODE` was refused ("cannot be used
/// with '--config'") while `loquax home join CODE --config X` worked. The
/// directory names an instance no daemon serves, so a run that read it fails
/// naming that instance: the value arrived, not just the parse.
#[cfg(feature = "tui")]
#[test]
fn config_is_taken_before_and_after_the_subcommand() {
    let config = tempfile::tempdir().expect("config dir");
    let instance = format!("loquax-config-probe-{}", std::process::id());
    std::fs::write(
        config.path().join("config"),
        format!("[reticulum]\n  instance_name = {instance}\n"),
    )
    .expect("config");
    let dir = config.path().to_str().expect("utf-8 path");
    let code = "lqxh1notacode";
    let runs: [&[&str]; 6] = [
        &["--config", dir, "send", NOWHERE, "hi"],
        &["send", "--config", dir, NOWHERE, "hi"],
        &["--config", dir, "fetch", "--timeout", "1"],
        &["fetch", "--timeout", "1", "--config", dir],
        &["--config", dir, "home", "join", code],
        &["home", "join", code, "--config", dir],
    ];
    for args in runs {
        // The instance is the verbose text's (1265), and it is what proves
        // the value of --config arrived.
        let verbose: Vec<&str> = args.iter().copied().chain(["--verbose"]).collect();
        let run = run(&verbose, b"");
        assert!(
            !run.stderr.contains("cannot be used with"),
            "{args:?}: {}",
            run.stderr
        );
        if args.contains(&"join") {
            // The code is refused before the config is read; the parse is
            // what this run is for.
            assert!(run.stderr.contains("home code"), "{args:?}: {}", run.stderr);
        } else {
            assert!(run.stderr.contains(&instance), "{args:?}: {}", run.stderr);
        }
    }
}

/// A first `loquax send` or `loquax fetch` on a machine with no loquax home
/// yet: the home is made, owner only, before anything opens a file in it.
/// Both took the delivery lock in a directory nobody had made and failed
/// with `delivery.lock: No such file or directory` before they sent (the
/// nightly of 2026-10-30, four regression cells, 1141).
#[cfg(unix)]
#[test]
fn a_first_send_or_fetch_makes_the_home_before_it_takes_the_lock() {
    let instance = format!("loquax-nothing-here-{}", std::process::id());
    for args in [
        vec!["send", NOWHERE, "body", "--instance", &instance],
        vec!["fetch", "--instance", &instance],
    ] {
        let parent = tempfile::tempdir().expect("parent");
        let home = parent.path().join("loquax");
        let output = Command::new("sh")
            .args(["-c", "umask 0002 && exec \"$0\" \"$@\"", LNMSG])
            .args(&args)
            // The instance is the verbose text's (1265), and it is what
            // proves the run got as far as the daemon.
            .arg("--verbose")
            .env("LNMSG_HOME", &home)
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .output()
            .expect("run loquax");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            !stderr.contains("delivery.lock"),
            "{args:?} failed on the lock, not on the missing daemon: {stderr}"
        );
        assert!(stderr.contains(&instance), "{args:?}: {stderr}");
        assert_eq!(mode(&home), 0o700, "{args:?}");
    }
}
