//! `loquax mailbox setup` (1266, walk finding 4) through the real binary,
//! over a temporary home, with no daemon: the wizard writes files and
//! prints a code, and neither needs one.
//!
//! Three runs: the five steps by flags, ending in a code that parses and
//! carries the fixed address, and a second run that keeps what is set and
//! shows a new code; the rendezvous by flags, whose code carries the
//! config's list; and the interactive shape, the questions answered by
//! typed lines on stdin. The system mailbox (sudo, systemctl, runuser) is
//! not run here: it is the host's, and its command lines are the library's
//! unit tests (`loquax/src/home/setup.rs`).

use std::path::Path;
use std::process::{Command, Output, Stdio};

use loquax::home::code::HomeCode;

const LOQUAX: &str = env!("CARGO_BIN_EXE_loquax");

/// The lnsd default config, which the port for phones goes into.
const RETICULUM_CONFIG: &str = "[reticulum]\n  share_instance = Yes\n\n[interfaces]\n\n";

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("rns")).expect("rns dir");
        std::fs::write(dir.path().join("rns/config"), RETICULUM_CONFIG).expect("rns config");
        Self { dir }
    }

    fn home(&self) -> std::path::PathBuf {
        self.dir.path().join("loquax")
    }

    fn rns(&self) -> std::path::PathBuf {
        self.dir.path().join("rns")
    }

    /// `loquax mailbox setup <args>` with `stdin` typed in, from a file so
    /// the run is one `output()` and leaves no child behind.
    fn setup(&self, args: &[&str], stdin: &str) -> Output {
        let typed = self.dir.path().join("typed");
        std::fs::write(&typed, stdin).expect("typed answers");
        Command::new(LOQUAX)
            .args(["mailbox", "setup", "--config"])
            .arg(self.rns())
            .args(args)
            .env("LNMSG_HOME", self.home())
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::from(
                std::fs::File::open(&typed).expect("open typed"),
            ))
            .output()
            .expect("loquax runs")
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The five step lines, in order, each once.
fn assert_five_steps(text: &str) {
    let mut at = 0;
    for step in ["1/5  ", "2/5  ", "3/5  ", "4/5  ", "5/5  "] {
        let found = text[at..]
            .lines()
            .position(|line| line.starts_with(step))
            .unwrap_or_else(|| panic!("no {step:?} after the last step:\n{text}"));
        at += text[at..]
            .lines()
            .take(found + 1)
            .map(|line| line.len() + 1)
            .sum::<usize>();
    }
}

/// The code the run printed: the line after step 4's head, parsed with the
/// list it carries.
fn printed_code(text: &str) -> (HomeCode, Vec<String>, String) {
    let line = text
        .lines()
        .skip_while(|line| !line.starts_with("4/5  "))
        .nth(1)
        .unwrap_or_else(|| panic!("no code line:\n{text}"))
        .to_string();
    let (code, list) = HomeCode::parse_with_rendezvous(&line)
        .unwrap_or_else(|error| panic!("{line:?} is no code: {error:?}"));
    (code, list, line)
}

/// A port for phones no other test asks for: the kernel's free one, let go
/// at once so the setup's own probe finds it free. The default port, 4243,
/// is the host's, and two tests of this file asking for it at the same
/// instant took it from each other (1311).
fn free_port() -> u16 {
    std::net::TcpListener::bind(("0.0.0.0", 0))
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port()
}

fn config(home: &Path) -> String {
    std::fs::read_to_string(home.join("config")).expect("config written")
}

#[test]
fn the_flags_set_up_a_mailbox_with_a_fixed_address_and_a_second_run_keeps_it() {
    let home = Home::new();
    // The address keeps 4243, as a router forwarding it here would; the
    // port the daemon listens on is the test's own.
    let port = free_port().to_string();
    let first = stdout(&home.setup(
        &[
            "--store",
            "account",
            "--address",
            "192.0.2.7:4243",
            "--phone-port",
            &port,
            "--name",
            "phone",
            "--yes",
        ],
        "",
    ));
    assert_five_steps(&first);
    assert!(
        first.contains("at the fixed address 192.0.2.7:4243\n"),
        "{first}"
    );
    assert!(
        first.contains(&format!("Added port {port} for phones")),
        "{first}"
    );
    assert!(first.contains('█'), "no QR code:\n{first}");
    assert!(
        first.contains("\"On a computer at home\", then \"Scan the code\""),
        "{first}"
    );
    let (code, list, line) = printed_code(&first);
    assert_eq!(list, ["192.0.2.7:4243"]);
    let rns = std::fs::read_to_string(home.rns().join("config")).expect("rns config");
    assert!(rns.contains("[[Loquax Mailbox TCP]]"), "{rns}");
    assert!(rns.contains(&format!("listen_port = {port}\n")), "{rns}");
    assert_eq!(
        config(&home.home()),
        "rendezvous_dial = off\nhome_address = 192.0.2.7:4243\n"
    );

    // Again with nothing given: the address is kept, the identity too, and
    // the code is a new one for the same mailbox.
    let second = stdout(&home.setup(&["--store", "account", "--yes"], ""));
    assert_five_steps(&second);
    assert!(
        second.contains("This mailbox is set up already"),
        "{second}"
    );
    assert!(
        second.contains("at the fixed address 192.0.2.7:4243 (as before)"),
        "{second}"
    );
    assert!(
        second.contains(&format!("The port for phones, {port}, is open.")),
        "{second}"
    );
    let (again, again_list, again_line) = printed_code(&second);
    assert_eq!(again.public_key, code.public_key);
    assert_ne!(again_line, line);
    assert_eq!(again_list, ["192.0.2.7:4243"]);

    // `mailbox admit` after it carries the address setup kept.
    let admit = Command::new(LOQUAX)
        .args(["mailbox", "admit"])
        .env("LNMSG_HOME", home.home())
        .output()
        .expect("admit");
    let admitted = stdout(&admit);
    assert!(admitted.contains("@192.0.2.7:4243\n"), "{admitted}");
}

#[test]
fn the_rendezvous_by_flags_puts_the_configs_list_in_the_code() {
    let home = Home::new();
    std::fs::create_dir_all(home.home()).expect("home");
    // A list of its own, so the shipped one (held back on a lab host) does
    // not decide; and an address from before, which the choice takes out.
    std::fs::write(
        home.home().join("config"),
        "rendezvous = rv.example:4343\nrendezvous_dial = off\nhome_address = 192.0.2.7:4243\n",
    )
    .expect("config");
    let text = stdout(&home.setup(
        &["--store", "account", "--reach", "rendezvous", "--yes"],
        "",
    ));
    assert_five_steps(&text);
    assert!(
        text.contains("from anywhere, through leviculum.network\n"),
        "{text}"
    );
    let (_, list, _) = printed_code(&text);
    assert_eq!(list, ["rv.example:4343"]);
    assert_eq!(
        config(&home.home()),
        "rendezvous = rv.example:4343\nrendezvous_dial = on\n"
    );
    // The rendezvous needs no port for phones, and refuses one.
    let rns = std::fs::read_to_string(home.rns().join("config")).expect("rns config");
    assert_eq!(rns, RETICULUM_CONFIG);
    let refused = home.setup(
        &[
            "--store",
            "account",
            "--reach",
            "rendezvous",
            "--phone-port",
            "5000",
            "--yes",
        ],
        "",
    );
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--phone-port goes with"));
}

#[test]
fn the_questions_take_typed_answers() {
    let home = Home::new();
    // Your account, a fixed address, this one; the port for phones is the
    // test's own.
    let port = free_port().to_string();
    let text = stdout(&home.setup(&["--phone-port", &port], "2\n2\n198.51.100.4:4243\n"));
    for question in [
        "1/5  Where your mailbox keeps your mail\n",
        "     2) In your own account, in ",
        "3/5  How your phone reaches it\n",
        "     1) From anywhere, through leviculum.network",
        "     2) At a fixed address you give",
        "     The address, host:port",
    ] {
        assert!(text.contains(question), "no {question:?}:\n{text}");
    }
    assert_five_steps(&text);
    assert!(
        text.contains(&format!("Added port {port} for phones")),
        "{text}"
    );
    let (_, list, _) = printed_code(&text);
    assert_eq!(list, ["198.51.100.4:4243"]);

    // A choice that is none is refused before anything is written.
    let refused = home.setup(&["--store", "account"], "7\n");
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("choose 1 or 2"));
    assert_eq!(
        config(&home.home()),
        "rendezvous_dial = off\nhome_address = 198.51.100.4:4243\n"
    );
}
