//! The messenger (`loquax`, and its `loquax tui` alias) through the real
//! binary, scripted, against a real daemon: open the conversation, type two lines (`Alt-Enter` between them), send,
//! watch the delivery mark move from `◷` to `✓✓`, read the two-line reply. The frames the script asks for are pinned in
//! `tests/fixtures/tui_session.frames`.
//!
//! The keymap's sessions (`docs/src/concepts/loquax-keys-and-layout.md`,
//! section 7, slice 1) need no correspondent: one driven only by vi
//! spellings and one only by Emacs spellings reach the same frames, the
//! enhanced and the plain keyboard reach the same two-line body, `Ctrl-c`
//! outside compose says what quits and the session goes on, and
//! `send_on_enter` off swaps `Enter` and `Alt-Enter`. Pinned in
//! `tests/fixtures/tui_keys.frames`.
//!
//! The chat layout's session (section 7, slice 2) runs one exchange at 80 x
//! 24 and at 120 x 40, the two mockups' sizes: a message from the peer on
//! one day, ours on it and on the next, the peer's two replies arriving while
//! the reader is scrolled up (the `new` rule above them, the view not moved),
//! back to the newest, a reply, and the detail view of our first message with
//! its proof. Pinned in `tests/fixtures/tui_chat.frames`. While the reader
//! is scrolled up, with both sides, a date, the `new` rule, a selected block
//! and the palette's `theme` toast on screen, the 80 x 24 session writes the
//! screen's style map in each tier and theme (section 5, "Colour", part 5),
//! pinned in `tests/fixtures/tui_chat.styles`; the frames stay the
//! monochrome ones.
//!
//! The mouse's sessions (section 7, slice 3) click a conversation, the
//! compose box, `send` on its border, a message, its mark and the footer's
//! `? help`; turn the wheel over the conversation while the list has the
//! focus; and drag across two messages, whose release copies them by OSC 52.
//! Pinned in `tests/fixtures/tui_mouse.frames`.
//!
//! The search's session (section 7, slice 4) runs at 120 x 40 and at 80 x
//! 24: four messages in two conversations, `/` and `tripod` (the messages
//! group, three), the third result opened at its message with the match
//! marked in place, `n` and `N` stepping the matches there, a query that
//! is a conversation's name and a word of a message (both groups), and
//! `Esc` bringing the list back with the conversation as it was. Pinned in
//! `tests/fixtures/tui_search_session.frames`.
//!
//! The names' session (section 7, slice 5) lists three correspondents by
//! address, then each attaches and announces: Bob's name replaces his
//! address in the list, the header and the compose box with its short hash;
//! a second peer announcing `B0b` marks both with `≈` and the header says
//! whose name it shares; and the third, whom the contacts file names
//! `Carol`, keeps that name over the `Caroline` it announces and is `known`.
//! Then the palette renames her (`rename`, the petname in the palette to
//! edit), blocks her (her conversation leaves the list) and lists the
//! blocked (`blocked`), and the contacts file says both, its comment kept.
//! Pinned in `tests/fixtures/tui_names.frames`.
//!
//! The history's session (section 7, slice 6) runs loquax three times over
//! one home: an exchange with Bob and a send to Carol, who never answers,
//! cut off by quitting; a restart with no `--to`, which lists both from the
//! store, Bob by the name he announced in the first run, our proven message
//! `✓✓` and the cut-off send `◷` with the restart in its ledger, then pins
//! Bob and deletes his reply (`d`, `y`); and a third start that shows the
//! pin above its rule and the reply gone. Pinned in
//! `tests/fixtures/tui_history.frames`.
//!
//! The draft's session (1219) runs loquax twice over one home: a line typed
//! to Bob and not sent stays when the reader goes on to Carol, Bob's row
//! saying `Draft: …` where its last line would be; after the restart Bob's
//! conversation is the store's only one, and its compose holds the line.
//! Pinned in `tests/fixtures/tui_draft.frames`.
//!
//! The telemetry session (`docs/src/concepts/loquax-map.md`, slice (a)) runs
//! at 80 x 24 and at 120 x 40: the peer writes a line, then sends two
//! position reports with no text, as Sideband and Columba do. The header
//! carries the position, at 120 columns with its accuracy and age, at 80
//! only `pos` and the age; the two reports fold into one `◎ position` line
//! and no empty block is drawn; `v` opens the detail view (`Enter` on a
//! report opens the map), which lists the sensors. Pinned in
//! `tests/fixtures/tui_telemetry.frames`.
//!
//! The mailbox schedule's session (`docs/src/concepts/loquax-mailbox.md`,
//! section 1, "When to sync") runs against a real propagation node, lnpnd's
//! engine on the same daemon: one message waits there before the start and
//! the start's sync collects it, a second is deposited while loquax runs and
//! arrives without a key, inside the activity cadence and well before the
//! bound. `--sync-interval 30s` scales the schedule down: a four-second
//! cadence for twenty seconds after the first message. No frames are pinned;
//! the counts and `tui.log`'s `SYNC` lines are what is under test.
//!
//! The ugly store's session (1173) opens a store shaped like a year of use
//! and two imports (`support/ugly_store.rs`), goes to the bottom of the
//! list, opens the import there and scrolls to its oldest row. Pinned in
//! `tests/fixtures/tui_ugly.frames`. The fetch sessions, whose frames are
//! not under test, run on the same store; the others pin frames of a
//! store they build themselves, so they keep it.
//!
//! The daemon is the in-process one `mailbox_cli.rs` uses. The correspondent
//! is a second loquax engine attached to it in this process
//! (`loquax::engine::attach`), a full LXMF router, so the proof that turns the
//! mark green is a real one. Both identities are fixed, which is what makes
//! the addresses on the screen, and so the frames, the same on every run.
//!
//! The order matters once: the correspondent attaches after the TUI is ready,
//! so its one announce reaches an engine that is already listening.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_core::identity_store::encode_identity;
use leviculum_std::Identity;
use loquax::clock::Tokio;
use loquax::effects::Terminal;
use loquax::engine::{attach, delivery_address, AttachConfig};
use loquax::fanout::Fanout;
use loquax::outbox::{OutboxEvent, SendRequest, Via};
use loquax::send::{run_send, SendOptions};

mod support;

#[path = "support/nomadnet_tree.rs"]
mod nomadnet_tree;

#[path = "support/lnpnd.rs"]
mod lnpnd_node;

#[path = "support/ugly_store.rs"]
mod ugly_store;

use support::daemon;

const LNMSG: &str = env!("CARGO_BIN_EXE_loquax");
const EXPECTED_CHAT: &str = include_str!("fixtures/tui_chat.frames");
const EXPECTED_CHAT_DE: &str = include_str!("fixtures/tui_chat_de.frames");
const EXPECTED_STYLES: &str = include_str!("fixtures/tui_chat.styles");

const SCRIPT: &str = "\
until ready 20
frame attached
# conversations -> messages -> compose
key enter
key i
type hello from the tui
newline
type on two lines
frame typed
key enter
frame queued
until delivery arrived 30
frame delivered
until received 1 30
frame reply
key esc
key q
";

/// A send to a correspondent that is not there yet: the row carries the
/// direct leg and its seconds, then the correspondent attaches and the proof
/// lands, and the script leaves in the pump that showed it.
const LATE_SCRIPT: &str = "\
until ready 20
frame attached
key enter
key i
type are you there
key enter
until leg direct 5
sleep 2500
frame waiting
until delivery arrived 30
frame delivered
key esc
key q
";

/// A session's frames under `tests/fixtures`, compared whole; frames that
/// moved are left in the temp dir as `loquax-golden/<name>`, and
/// `LOQUAX_WRITE_GOLDEN=1` writes them for a change that means to move
/// them.
fn same_session(name: &str, got: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, got).expect("write the fixture");
        return;
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(got) {
        let dir = std::env::temp_dir().join("loquax-golden");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join(name), got).expect("write");
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             loquax/tests/fixtures/{name}\n{got}",
            dir.join(name).display()
        );
    }
}

fn fixed_identity(seed: u8) -> Identity {
    Identity::from_private_key_bytes(&[seed; 64]).expect("a 64-byte key")
}

/// Wait for the frames file to hold a frame with this label.
async fn wait_for_frame(frames: &Path, label: &str, within: Duration) {
    let deadline = Instant::now() + within;
    let marker = format!(": {label} ---");
    loop {
        let text = std::fs::read_to_string(frames).unwrap_or_default();
        if text.contains(&marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the TUI never drew {label:?}; frames so far:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scripted_session_sends_sees_the_proof_and_reads_the_reply() {
    same_session(
        "tui_session.frames",
        &a_scripted_session_sends_sees_the_proof_and_reads_the_reply_in("").await,
    );
}

/// [`a_scripted_session_sends_sees_the_proof_and_reads_the_reply`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_scripted_session_sends_sees_the_proof_and_reads_the_reply_in_german() {
    let frames = a_scripted_session_sends_sees_the_proof_and_reads_the_reply_in("lang de\n").await;
    assert!(frames.contains("du: "), "{frames}");
    same_session("tui_session_de.frames", &frames);
}

/// The session of [`a_scripted_session_sends_sees_the_proof_and_reads_the_reply`] with `lang` before every script: its frames.
async fn a_scripted_session_sends_sees_the_proof_and_reads_the_reply_in(lang: &str) -> String {
    let en = lang.is_empty();
    let daemon = daemon("tui", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let our_address = delivery_address(&ours).expect("our address");
    let theirs = fixed_identity(0x22);
    let their_address = delivery_address(&theirs).expect("their address");
    let their_hex: String = their_address.iter().map(|b| format!("{b:02x}")).collect();

    let script = home.path().join("session.script");
    let frames = home.path().join("session.frames");
    std::fs::write(&script, format!("{lang}{SCRIPT}")).expect("write the script");

    let args: Vec<String> = vec![
        "tui".into(),
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        their_hex,
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x16".into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    wait_for_frame(&frames, "attached", Duration::from_secs(20)).await;
    let storage = tempfile::tempdir().expect("their storage");
    let mut them = attach(AttachConfig {
        instance: daemon.instance.clone(),
        config_dir: Some(daemon.config_dir.path().to_path_buf()),
        storage_dir: storage.path().to_path_buf(),
        identity: theirs,
        display_name: b"them".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("the correspondent attaches");

    wait_for_frame(&frames, "delivered", Duration::from_secs(40)).await;
    let reply = run_send(
        &mut them.outbox,
        SendRequest {
            destination: our_address,
            title: Vec::new(),
            body: b"hello back\nalso on two lines".to_vec(),
            fields: Vec::new(),
            via: Via::Direct,
        },
        &SendOptions::new(daemon.instance.clone(), Duration::from_secs(20)),
        &Tokio::new(),
    )
    .await;
    assert!(!en || reply.is_ok(), "the reply did not go out: {reply:?}");

    let output = child.await.expect("the loquax child thread");
    them.stop().await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {stderr}\nframes:\n{got}"
    );
    got
}

/// The leg label's seconds come from the script's clock: `until leg` applies
/// no tick and `sleep 2500` applies ten, one per 250 ms of its own length,
/// so the row reads `direct 2 s` however long the host took. And a script
/// that quits on the proof still leaves the direct leg's `sending` and
/// `proof` lines in `tui.log`: before leviculum 1014 the runner was cut off
/// before it read them (the late-recipient arm of periculum 1013).
#[tokio::test(flavor = "multi_thread")]
async fn a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps() {
    same_session(
        "tui_late.frames",
        &a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps_in("").await,
    );
}

/// [`a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps_in_german() {
    let frames =
        a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps_in("lang de\n").await;
    assert!(frames.contains("du: "), "{frames}");
    same_session("tui_late_de.frames", &frames);
}

/// The session of [`a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps`] with `lang` before every script: its frames.
async fn a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps_in(
    lang: &str,
) -> String {
    let daemon = daemon("tui-late", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let theirs = fixed_identity(0x22);
    let their_address = delivery_address(&theirs).expect("their address");
    let their_hex: String = their_address.iter().map(|b| format!("{b:02x}")).collect();

    let script = home.path().join("late.script");
    let frames = home.path().join("late.frames");
    std::fs::write(&script, format!("{lang}{LATE_SCRIPT}")).expect("write the script");
    // The bare form, plain `loquax` with the messenger's options; the session
    // above runs the `tui` alias, so both spellings reach a daemon.
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        their_hex.clone(),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x16".into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    // Only once the label is on screen does the correspondent appear.
    wait_for_frame(&frames, "waiting", Duration::from_secs(30)).await;
    let storage = tempfile::tempdir().expect("their storage");
    let them = attach(AttachConfig {
        instance: daemon.instance.clone(),
        config_dir: Some(daemon.config_dir.path().to_path_buf()),
        storage_dir: storage.path().to_path_buf(),
        identity: theirs,
        display_name: b"them".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("the correspondent attaches");

    let output = child.await.expect("the loquax child thread");
    them.stop().await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {stderr}\nframes:\n{got}\ntui.log:\n{log}"
    );

    let to = format!("to={}", &their_hex[..8]);
    let steps: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("SEND leg=direct") && line.contains(&to))
        .filter_map(|line| line.split(" state=").nth(1))
        .filter_map(|rest| rest.split(' ').next())
        .collect();
    assert_eq!(
        steps,
        ["path_wait", "sending", "proof"],
        "the direct leg's steps in tui.log:\n{log}"
    );

    got
}

/// The vi reader's session: `j`/`k`/`G`/`g`, `i` to write, arrows to edit,
/// `Esc` back out, `:` for the palette.
const VI_SCRIPT: &str = "\
until ready 20
frame attached
key j
frame second
key k
key G
key g
frame first
key enter
frame messages
key i
type helo
key left
type l
frame compose
key esc
key esc
frame list
key :
type help
key enter
frame help
key esc
frame closed
key q
";

/// The same session in Emacs spellings: `Ctrl-n`/`Ctrl-p`, `Alt->`/`Alt-<`,
/// `Ctrl-b` to edit, `Ctrl-g` back out, `Alt-x` for the palette. Compose is
/// `Tab`, since Emacs has no key that means "start writing".
const EMACS_SCRIPT: &str = "\
until ready 20
frame attached
key ctrl-n
frame second
key ctrl-p
key alt->
key alt-<
frame first
key enter
frame messages
key tab
type helo
key ctrl-b
type l
frame compose
key ctrl-g
key ctrl-g
frame list
key alt-x
type help
key enter
frame help
key ctrl-g
frame closed
key ctrl-q
";

/// The enhanced world: `Shift-Enter` breaks the line, `Ctrl-Enter` sends,
/// and `Ctrl-c` outside compose says what quits without quitting.
const ENHANCED_SCRIPT: &str = "\
until ready 20
keyboard enhanced
key enter
key i
type one
key shift-enter
type two
frame body
key ctrl-enter
frame sent
key esc
key esc
key ctrl-c
frame ctrl-c
key j
frame still here
key q
";

/// The plain world: `Alt-Enter` breaks the line.
const PLAIN_ALT_SCRIPT: &str = "\
until ready 20
key enter
key i
type one
key alt-enter
type two
frame body
key esc
key esc
key q
";

/// The plain world: `Ctrl-j` breaks the line too.
const PLAIN_CTRL_J_SCRIPT: &str = "\
until ready 20
key enter
key i
type one
key ctrl-j
type two
frame body
key esc
key esc
key q
";

/// `send_on_enter` off from the palette: `Enter` breaks the line,
/// `Alt-Enter` sends, and the hint says so.
const ENTER_OFF_SCRIPT: &str = "\
until ready 20
key :
type send on enter
key enter
frame switched
key enter
key i
type one
key enter
type two
frame body
key alt-enter
frame sent
key esc
key esc
key q
";

/// Run one script against `daemon` with two correspondents that never
/// answer, and hand back its frames.
async fn run_keys_script(daemon: &support::Daemon, name: &str, script: &str) -> String {
    run_script_at(daemon, name, script, "80x16").await.0
}

/// The same at `size`, with `tui.log` beside the frames.
async fn run_script_at(
    daemon: &support::Daemon,
    name: &str,
    script: &str,
    size: &str,
) -> (String, String) {
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    let peer = |seed: u8| -> String {
        let address = delivery_address(&fixed_identity(seed)).expect("an address");
        address.iter().map(|b| format!("{b:02x}")).collect()
    };
    let script_path = home.path().join(format!("{name}.script"));
    let frames = home.path().join(format!("{name}.frames"));
    std::fs::write(&script_path, script).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        peer(0x22),
        "--to".into(),
        peer(0x33),
        "--script".into(),
        script_path.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        size.into(),
    ];
    let child_home = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    })
    .await
    .expect("the loquax child thread");
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{name}: stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The script runner says once which world it starts in.
    assert!(
        log.contains("KEYS enhancement=no newline=Alt-Enter send=Enter"),
        "{name}: tui.log:\n{log}"
    );
    (got, log)
}

/// A frame without the compose pane's border, which carries the hint.
fn without_hint(frame: &str) -> Vec<&str> {
    frame
        .lines()
        .filter(|line| !line.contains(" send · "))
        .collect()
}

/// The frame labelled `label` in a frames file.
fn frame<'a>(frames: &'a str, label: &str) -> &'a str {
    let marker = format!(": {label} ---\n");
    let start = frames
        .find(&marker)
        .map(|at| at + marker.len())
        .unwrap_or_else(|| panic!("no frame {label:?} in:\n{frames}"));
    let end = frames[start..]
        .find("--- frame ")
        .map_or(frames.len(), |at| start + at);
    &frames[start..end]
}

#[tokio::test(flavor = "multi_thread")]
async fn the_keymap_sessions_reach_the_same_screens_in_every_spelling() {
    same_session(
        "tui_keys.frames",
        &the_keymap_sessions_reach_the_same_screens_in_every_spelling_in("").await,
    );
}

/// [`the_keymap_sessions_reach_the_same_screens_in_every_spelling`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn the_keymap_sessions_reach_the_same_screens_in_every_spelling_in_german() {
    let frames = the_keymap_sessions_reach_the_same_screens_in_every_spelling_in("lang de\n").await;
    assert!(frames.contains("du: "), "{frames}");
    same_session("tui_keys_de.frames", &frames);
}

/// The session of [`the_keymap_sessions_reach_the_same_screens_in_every_spelling`] with `lang` before every script: its frames.
async fn the_keymap_sessions_reach_the_same_screens_in_every_spelling_in(lang: &str) -> String {
    let en = lang.is_empty();
    let daemon = daemon("tui-keys", |b| b).await;
    let mut all = String::new();
    let mut sessions = Vec::new();
    for (name, script) in [
        ("vi", VI_SCRIPT),
        ("emacs", EMACS_SCRIPT),
        ("enhanced", ENHANCED_SCRIPT),
        ("plain-alt-enter", PLAIN_ALT_SCRIPT),
        ("plain-ctrl-j", PLAIN_CTRL_J_SCRIPT),
        ("enter-off", ENTER_OFF_SCRIPT),
    ] {
        let frames = run_keys_script(&daemon, name, &format!("{lang}{script}")).await;
        all.push_str(&format!("=== {name} ===\n{frames}"));
        sessions.push(frames);
    }
    let [vi, emacs, enhanced, plain_alt, plain_ctrl_j, enter_off] = sessions
        .try_into()
        .unwrap_or_else(|_| panic!("six sessions"));

    // vi and Emacs reach the same screens, frame for frame.
    assert_eq!(vi, emacs, "the vi and the Emacs session differ");

    // The two-line body is the same in both worlds; only the hint differs,
    // and it names the world's own chord.
    let body = frame(&enhanced, "body");
    assert!(
        !en || body.contains("Enter send · Shift-Enter newline"),
        "{body}"
    );
    let plain = frame(&plain_alt, "body");
    assert!(
        !en || plain.contains("Enter send · Alt-Enter newline"),
        "{plain}"
    );
    assert_eq!(without_hint(body), without_hint(plain));
    assert_eq!(plain, frame(&plain_ctrl_j, "body"));
    assert!(
        !en || frame(&enhanced, "sent").contains("you: one"),
        "{enhanced}"
    );

    // Ctrl-c outside compose says what quits, and the session goes on.
    assert!(
        !en || frame(&enhanced, "ctrl-c").contains("Ctrl-q quits"),
        "{enhanced}"
    );
    assert!(
        !en || frame(&enhanced, "still here").contains("● tui"),
        "{enhanced}"
    );

    // send_on_enter off: Enter broke the line, Alt-Enter sent.
    let off = frame(&enter_off, "body");
    assert!(
        !en || off.contains("Alt-Enter send · Enter newline"),
        "{off}"
    );
    assert!(!en || off.contains("│one"), "{off}");
    assert!(
        !en || frame(&enter_off, "sent").contains("you: one"),
        "{enter_off}"
    );

    all
}

/// 1381, finding 22: a script speaks English whatever the host's locale,
/// and its `lang` step switches the screen to German: the list then says
/// `du:` before our own line. Both sessions run the same keys, so the one
/// without the step is the control.
#[tokio::test(flavor = "multi_thread")]
async fn a_script_speaks_english_until_its_lang_step_says_german() {
    let daemon = daemon("tui-lang", |b| b).await;
    let keys = "\
until ready 20
key enter
key i
type eins
key enter
frame sent
key esc
key esc
key q
";
    let english = run_keys_script(&daemon, "lang-en", keys).await;
    let german = run_keys_script(&daemon, "lang-de", &format!("lang de\n{keys}")).await;
    let sent = frame(&english, "sent");
    assert!(sent.contains("you: eins"), "{english}");
    assert!(!sent.contains("du: eins"), "{english}");
    let gesendet = frame(&german, "sent");
    assert!(gesendet.contains("du: eins"), "{german}");
    assert!(!gesendet.contains("you: eins"), "{german}");
}

/// The chat session. Every time on screen comes from a `clock` step, and the
/// test sends each of the peer's messages only once the frame before it is
/// drawn, so each arrives under its own clock.
const CHAT_SCRIPT: &str = "\
until ready 20
frame attached
clock 2026-10-24 13:58
until received 1 30
clock 2026-10-24 14:02
key enter
key i
type yes, from ten. Shall I bring the T114 and the long antenna?
key enter
until delivery arrived 30
clock 2026-10-26 09:12
type on my way
key enter
until delivery arrived 30
key esc
key k
key k
tick 12
frame reading
clock 2026-10-26 14:04
until received 2 30
tick 12
frame first reply
clock 2026-10-26 14:05
until received 3 30
tick 12
frame new while reading
key :
type theme
key enter
tier truecolor
theme dark
styles truecolor dark
theme light
styles truecolor light
tier 256
theme dark
styles 256 dark
theme light
styles 256 light
tier 16
theme dark
styles 16 dark
theme light
styles 16 light
tier mono
styles mono
theme dark
tick 12
key G
frame newest
clock 2026-10-26 14:06
key i
type the tripod fits
key enter
until delivery arrived 30
tick 12
frame replied
key esc
key g
key j
key enter
frame detail
key esc
key q
";

/// A frame's lines with the message ids, which hash the LXMF timestamp and so
/// differ on every run, written as `x`s of the same width.
fn without_ids(frames: &str) -> String {
    let mut out = String::new();
    for line in frames.lines() {
        let masked = match (line.find("id     "), line.find("router as ")) {
            (Some(at), _) => {
                let start = at + "id     ".len();
                format!(
                    "{}{}{}",
                    &line[..start],
                    "x".repeat(64),
                    &line[start + 64..]
                )
            }
            (None, Some(at)) => {
                let start = at + "router as ".len();
                format!("{}{}{}", &line[..start], "x".repeat(8), &line[start + 8..])
            }
            (None, None) => line.to_string(),
        };
        out.push_str(&masked);
        out.push('\n');
    }
    out
}

/// One chat session at `size` against its own daemon, the peer answering on
/// the script's frames. The frames, and the style maps the script wrote
/// beside them.
async fn chat_session(size: &str) -> (String, String) {
    chat_session_with(size, CHAT_SCRIPT).await
}

/// [`chat_session`] with its own script: the German set (1447) runs the
/// same session after a `lang de` step.
async fn chat_session_with(size: &str, chat_script: &str) -> (String, String) {
    let daemon = daemon(&format!("tui-chat-{size}"), |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let our_address = delivery_address(&ours).expect("our address");
    let theirs = fixed_identity(0x22);
    let hex = |seed: u8| -> String {
        let address = delivery_address(&fixed_identity(seed)).expect("an address");
        address.iter().map(|b| format!("{b:02x}")).collect()
    };
    let script = home.path().join("chat.script");
    let frames = home.path().join("chat.frames");
    std::fs::write(&script, chat_script).expect("write the script");
    // A second correspondent that never answers, so the list has two rows.
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        hex(0x22),
        "--to".into(),
        hex(0x33),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        size.into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    wait_for_frame(&frames, "attached", Duration::from_secs(20)).await;
    let storage = tempfile::tempdir().expect("their storage");
    let mut them = attach(AttachConfig {
        instance: daemon.instance.clone(),
        config_dir: Some(daemon.config_dir.path().to_path_buf()),
        storage_dir: storage.path().to_path_buf(),
        identity: theirs,
        display_name: b"them".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("the correspondent attaches");
    let options = SendOptions::new(daemon.instance.clone(), Duration::from_secs(20));
    // The engine says `Ready` once, and each send waits for it: the sends go
    // through taps of one fan-out, as the TUI's own runners do.
    let mut fanout = Fanout::default();
    for (after, body) in [
        (None, "are you at the field site tomorrow?"),
        (Some("reading"), "ok, bring the T114."),
        (
            Some("first reply"),
            "and the small tripod\nif it fits in the bag",
        ),
    ] {
        if let Some(label) = after {
            wait_for_frame(&frames, label, Duration::from_secs(60)).await;
        }
        let mut tap = fanout.tap();
        let clock = Tokio::new();
        let send = run_send(
            &mut tap,
            SendRequest {
                destination: our_address,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: Vec::new(),
                via: Via::Direct,
            },
            &options,
            &clock,
        );
        tokio::pin!(send);
        let sent = loop {
            tokio::select! {
                sent = &mut send => break sent,
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    let _ = fanout.pump(&mut them.outbox);
                }
            }
        };
        assert!(sent.is_ok(), "{body:?} did not go out: {sent:?}");
    }

    let output = child.await.expect("the loquax child thread");
    them.stop().await;
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{size}: stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    let styles = std::fs::read_to_string(home.path().join("chat.frames.styles"))
        .expect("read the style maps");
    (got, styles)
}

/// One map of a styles file, by its label.
fn styles_of<'a>(styles: &'a str, label: &str) -> &'a str {
    let marker = format!("--- styles: {label} ---\n");
    let start = styles
        .find(&marker)
        .unwrap_or_else(|| panic!("no style map {label:?} in:\n{styles}"))
        + marker.len();
    let end = styles[start..]
        .find("--- styles:")
        .map_or(styles.len(), |end| start + end);
    &styles[start..end]
}

/// The chat session in German (1447, walk 1 finding 22): what a reader
/// whose desktop runs `LANG=de_DE.UTF-8` sees, so the next walk reads
/// German frames. A script plays its own terminal, English until a `lang`
/// step, so the set runs the chat script after `lang de`, the step the
/// locale takes for a real terminal. Pinned in
/// `tests/fixtures/tui_chat_de.frames`.
#[tokio::test(flavor = "multi_thread")]
async fn the_chat_in_german_is_its_own_golden_set() {
    let (frames, _) = chat_session_with("80x24", &format!("lang de\n{CHAT_SCRIPT}")).await;
    let all = without_ids(&frames);
    let attached = frame(&frames, "attached");
    assert!(attached.contains("Noch keine Nachrichten"), "{attached}");
    assert!(!attached.contains("nothing earlier"), "{attached}");
    if all != EXPECTED_CHAT_DE {
        let kept = std::env::temp_dir().join("loquax-tui-chat-de.frames");
        let _ = std::fs::write(&kept, &all);
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             loquax/tests/fixtures/tui_chat_de.frames\n{all}",
            kept.display()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_chat_layout_draws_the_mockups_at_both_sizes() {
    let mut all = String::new();
    let mut maps = String::new();
    for size in ["80x24", "120x40"] {
        let (frames, styles) = chat_session(size).await;
        if size == "80x24" {
            maps = styles;
        }
        // Read while scrolled up: the rule above the replies, and the block
        // being read still selected and on screen. (The whole exchange fits
        // both panes, so it is drawn from the bottom and grows upward.)
        let reading = frame(&frames, "reading");
        let first = frame(&frames, "first reply");
        let new = frame(&frames, "new while reading");
        assert!(first.contains(" 1 new "), "{size}: {first}");
        assert!(new.contains(" 2 new "), "{size}: {new}");
        let selected = |f: &str| -> Vec<String> {
            f.lines()
                .filter_map(|row| row.split_once("││").map(|(_, pane)| pane))
                .filter(|pane| pane.contains('▌'))
                .map(str::to_string)
                .collect()
        };
        assert!(!selected(reading).is_empty(), "{size}: {reading}");
        assert_eq!(selected(reading), selected(new), "{size}: the cursor moved");
        assert!(frame(&frames, "newest").contains("if it fits in the bag"));
        let detail = frame(&frames, "detail");
        assert!(
            detail.contains("arrived: proof received"),
            "{size}: {detail}"
        );
        assert!(detail.contains("from you to them"), "{size}: {detail}");
        assert!(!detail.contains("from you to them ·"), "{size}: {detail}");
        all.push_str(&format!("=== {size} ===\n{}", without_ids(&frames)));
    }
    if all != EXPECTED_CHAT {
        let kept = std::env::temp_dir().join("loquax-tui-chat.frames");
        let _ = std::fs::write(&kept, &all);
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             loquax/tests/fixtures/tui_chat.frames\n{all}",
            kept.display()
        );
    }

    // The colour of the same screen (loquax-keys-and-layout.md, section 5,
    // "Colour", part 5): six maps, one a tier and theme, and the monochrome
    // one. They differ where the table differs.
    let has = |label: &str, letter: char| styles_of(&maps, label).contains(letter);
    for label in [
        "truecolor dark",
        "truecolor light",
        "256 dark",
        "256 light",
        "16 dark",
        "16 light",
    ] {
        for letter in ['.', '^', '!'] {
            assert!(has(label, letter), "{label}: no {letter:?}");
        }
    }
    for label in ["truecolor dark", "truecolor light", "256 dark", "256 light"] {
        for letter in [',', '=', 't'] {
            assert!(has(label, letter), "{label}: no {letter:?}");
        }
    }
    // In 16 colours their blocks are untinted on dark, and the toast is
    // reverse video in both.
    for letter in [',', '=', 't'] {
        assert!(!has("16 dark", letter), "16 dark: {letter:?}");
    }
    assert!(has("16 light", ','), "16 light: their blocks");
    assert!(!has("16 light", 't'), "16 light: the toast");
    assert!(
        styles_of(&maps, "mono")
            .chars()
            .all(|c| matches!(c, ' ' | '^' | '\n')),
        "monochrome holds only blanks and reverse video:\n{}",
        styles_of(&maps, "mono")
    );
    if maps != EXPECTED_STYLES {
        let kept = std::env::temp_dir().join("loquax-tui-chat.styles");
        let _ = std::fs::write(&kept, &maps);
        panic!(
            "the style maps moved; this run's are in {}, the pinned ones in \
             loquax/tests/fixtures/tui_chat.styles\n{maps}",
            kept.display()
        );
    }
}

/// The mouse (section 7, slice 3), by clicks alone: the second
/// conversation's row, the compose box, `send` on its border (the send
/// goes out: its leg is in `tui.log`), the message, its mark (the detail
/// view), outside the view (closed), and the footer's `? help`. Cells count
/// from 1, as the frames do.
const CLICK_SCRIPT: &str = "\
until ready 20
frame attached
click 5 5
frame second conversation
click 40 22
type hello by mouse
frame typed
click 55 21
frame sent by click
click 60 20
frame message selected
click 78 20
frame detail
click 2 2
frame closed
click 62 24
frame help
key esc
key q
";

/// The wheel over the conversation while the list has the focus: the
/// conversation scrolls six rows for two notches, the focus and the list
/// stay, and two notches back are the newest again.
const WHEEL_SCRIPT: &str = "\
until ready 20
key i
type message 1
key enter
type message 2
key enter
type message 3
key enter
type message 4
key enter
type message 5
key enter
type message 6
key enter
type message 7
key enter
type message 8
key enter
type message 9
key enter
type message 10
key enter
key esc
key esc
frame list focused
wheel up 50 10 2
frame wheeled up
wheel down 50 10 2
frame wheeled down
key q
";

/// A drag across two of our messages copies their text by OSC 52, without
/// the times and the marks.
const DRAG_SCRIPT: &str = "\
until ready 20
key i
type first line
key enter
type second line
key enter
key esc
frame two sent
drag 59 18 69 20
frame dragged
tick 12
frame selected
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn the_mouse_clicks_wheels_and_drags_through_the_screen() {
    same_session(
        "tui_mouse.frames",
        &the_mouse_clicks_wheels_and_drags_through_the_screen_in("").await,
    );
}

/// [`the_mouse_clicks_wheels_and_drags_through_the_screen`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn the_mouse_clicks_wheels_and_drags_through_the_screen_in_german() {
    let frames = the_mouse_clicks_wheels_and_drags_through_the_screen_in("lang de\n").await;
    assert!(frames.contains("du: "), "{frames}");
    same_session("tui_mouse_de.frames", &frames);
}

/// The session of [`the_mouse_clicks_wheels_and_drags_through_the_screen`] with `lang` before every script: its frames.
async fn the_mouse_clicks_wheels_and_drags_through_the_screen_in(lang: &str) -> String {
    let en = lang.is_empty();
    let daemon = daemon("tui-mouse", |b| b).await;
    let mut all = String::new();
    let mut runs = Vec::new();
    for (name, script) in [
        ("click", CLICK_SCRIPT),
        ("wheel", WHEEL_SCRIPT),
        ("drag", DRAG_SCRIPT),
    ] {
        let (frames, log) = run_script_at(&daemon, name, &format!("{lang}{script}"), "80x24").await;
        all.push_str(&format!("=== {name} ===\n{frames}"));
        runs.push((frames, log));
    }
    let [(click, click_log), (wheel, _), (drag, _)] =
        runs.try_into().unwrap_or_else(|_| panic!("three sessions"));

    // The click on the second row opened it; the one on `send` sent, the
    // runner's first leg is in the log.
    assert!(
        !en || frame(&click, "second conversation").contains("│▌Unknown ·5ec0"),
        "{click}"
    );
    assert!(
        !en || frame(&click, "sent by click").contains("hello by mouse  12:00 ◷"),
        "{click}"
    );
    assert!(
        !en || click_log.contains("SEND leg=direct to=5ec0ea2e"),
        "tui.log:\n{click_log}"
    );
    assert!(!en || frame(&click, "message selected").contains("▌hello by mouse"));
    assert!(!en || frame(&click, "detail").contains("from you to Unknown ·5ec0"));
    assert!(!en || !frame(&click, "closed").contains("from you to"));
    assert!(
        !en || frame(&click, "help").contains("Loquax help"),
        "{click}"
    );

    // The wheel moved the conversation and nothing else.
    let up = frame(&wheel, "wheeled up");
    assert_ne!(up, frame(&wheel, "list focused"));
    assert!(!en || up.contains("● tui"), "the list kept the focus: {up}");
    assert!(!en || !up.contains("message 10  12:00"), "{up}");
    assert_eq!(frame(&wheel, "wheeled down"), frame(&wheel, "list focused"));

    // The drag's release copied the two lines and only them.
    let copied = format!(
        "--- osc52 {} ---",
        leviculum_ui::clipboard::osc52("first line\nsecond line").replace('\x1b', "\\e")
    );
    assert!(!en || drag.contains(&copied), "{copied}\n{drag}");
    assert!(
        !en || frame(&drag, "dragged").contains("copied 22 chars"),
        "{drag}"
    );
    all
}

/// The search, by keys alone, against correspondents that never answer: our
/// messages are what it finds. No step waits on the engine after `ready`,
/// so every frame is the same on every run.
const SEARCH_SCRIPT: &str = "\
until ready 20
clock 2026-10-23 19:40
key enter
key i
type did the board come back from the soak?
key enter
clock 2026-10-26 14:05
type and the small tripod
newline
type if it fits in the bag
key enter
clock 2026-10-26 14:06
type the tripod fits
key enter
key esc
key esc
key j
key enter
key i
clock 2026-10-26 14:07
type the tripod stays in the van, 61ddb79a has the bag
key enter
key esc
key esc
key /
type tripod
frame typed
key tab
key j
key j
key enter
frame opened
key n
frame next
key N
frame previous
key /
key backspace
key backspace
key backspace
key backspace
key backspace
key backspace
type 61dd
frame both groups
key esc
frame closed
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn the_search_finds_opens_and_steps_in_place_at_both_sizes() {
    same_session(
        "tui_search_session.frames",
        &the_search_finds_opens_and_steps_in_place_at_both_sizes_in("").await,
    );
}

/// [`the_search_finds_opens_and_steps_in_place_at_both_sizes`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn the_search_finds_opens_and_steps_in_place_at_both_sizes_in_german() {
    let frames = the_search_finds_opens_and_steps_in_place_at_both_sizes_in("lang de\n").await;
    assert!(frames.contains("du: "), "{frames}");
    same_session("tui_search_session_de.frames", &frames);
}

/// The session of [`the_search_finds_opens_and_steps_in_place_at_both_sizes`] with `lang` before every script: its frames.
async fn the_search_finds_opens_and_steps_in_place_at_both_sizes_in(lang: &str) -> String {
    let en = lang.is_empty();
    let daemon = daemon("tui-search", |b| b).await;
    let mut all = String::new();
    for size in ["120x40", "80x24"] {
        let (frames, _) = run_script_at(
            &daemon,
            &format!("search-{size}"),
            &format!("{lang}{SEARCH_SCRIPT}"),
            size,
        )
        .await;
        let typed = frame(&frames, "typed");
        assert!(!en || typed.contains("● Search"), "{size}: {typed}");
        assert!(!en || typed.contains(" esc closes "), "{size}: {typed}");
        assert!(!en || typed.contains("│ / tripod"), "{size}: {typed}");
        assert!(!en || typed.contains("│ MESSAGES "), "{size}: {typed}");
        assert!(
            !en || !typed.contains("│ CONVERSATIONS "),
            "{size}: {typed}"
        );
        assert!(!en || typed.contains("Esc close search"), "{size}: {typed}");
        // The third result, opened at its message: the block selected and
        // the match marked in place.
        let opened = frame(&frames, "opened");
        assert!(
            !en || opened.contains("▌and the small «tripod»"),
            "{size}: {opened}"
        );
        assert!(
            !en || opened.contains("the «tripod» fits"),
            "{size}: {opened}"
        );
        assert!(
            !en || opened.contains("n / N next, previous"),
            "{size}: {opened}"
        );
        assert!(
            !en || frame(&frames, "next").contains("▌the «tripod» fits"),
            "{size}"
        );
        assert_eq!(opened, frame(&frames, "previous"), "{size}: N went back");
        let both = frame(&frames, "both groups");
        assert!(!en || both.contains("│ CONVERSATIONS "), "{size}: {both}");
        assert!(!en || both.contains("│ MESSAGES "), "{size}: {both}");
        // Found by its address, which the row shows as the tail of a
        // shared "Unknown" (1452).
        assert!(!en || both.contains("Unknown ·«61dd»"), "{size}: {both}");
        // Closed: the list and its `/ search` back, nothing marked, the
        // conversation still the one the result opened.
        let closed = frame(&frames, "closed");
        assert!(!en || closed.contains("● tui"), "{size}: {closed}");
        assert!(!en || closed.contains(" / search "), "{size}: {closed}");
        assert!(!en || !closed.contains('«'), "{size}: {closed}");
        assert!(
            !en || closed.contains("Unknown ·61dd  Two contacts"),
            "{size}: {closed}"
        );
        all.push_str(&format!("=== {size} ===\n{frames}"));
    }
    all
}

/// Each correspondent attaches only after the screen has drawn the frame
/// before its own, so its one announce reaches an engine that is listening,
/// and `until named` holds the script until the name is there.
const NAMES_SCRIPT: &str = "\
until ready 20
frame attached
until named 1 30
frame named
until named 2 30
frame collision
until named 3 30
key j
key j
frame petname
key :
type rename
key enter
key ctrl-u
type Carol Meyer
frame rename
key enter
frame renamed
key :
type block
key enter
frame blocked
key :
type blocked
key enter
frame blocked view
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn names_from_announces_are_alone_collide_with_their_tail_and_yield_to_a_petname() {
    same_session(
        "tui_names.frames",
        &names_from_announces_are_alone_collide_with_their_tail_and_yield_to_a_petname_in("").await,
    );
}

/// [`names_from_announces_are_alone_collide_with_their_tail_and_yield_to_a_petname`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn names_from_announces_are_alone_collide_with_their_tail_and_yield_to_a_petname_in_german() {
    let frames = names_from_announces_are_alone_collide_with_their_tail_and_yield_to_a_petname_in(
        "lang de\n",
    )
    .await;
    assert!(frames.contains("Zwei Kontakte"), "{frames}");
    same_session("tui_names_de.frames", &frames);
}

/// The session of [`names_from_announces_are_alone_collide_with_their_tail_and_yield_to_a_petname`] with `lang` before every script: its frames.
async fn names_from_announces_are_alone_collide_with_their_tail_and_yield_to_a_petname_in(
    lang: &str,
) -> String {
    let en = lang.is_empty();
    let daemon = daemon("tui-names", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    let hex = |seed: u8| -> String {
        let address = delivery_address(&fixed_identity(seed)).expect("an address");
        address.iter().map(|b| format!("{b:02x}")).collect()
    };
    std::fs::write(
        home.path().join("contacts"),
        format!("# the reader's own names\nname {} Carol\n", hex(0x44)),
    )
    .expect("write the contacts");
    let script = home.path().join("names.script");
    let frames = home.path().join("names.frames");
    std::fs::write(&script, format!("{lang}{NAMES_SCRIPT}")).expect("write the script");
    let mut args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
    ];
    for seed in [0x22, 0x33, 0x44] {
        args.extend(["--to".into(), hex(seed)]);
    }
    args.extend([
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x16".into(),
    ]);
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    let mut attached = Vec::new();
    let mut storages = Vec::new();
    for (seed, name, after) in [
        (0x22, "Bob", "attached"),
        (0x33, "B0b", "named"),
        (0x44, "Caroline", "collision"),
    ] {
        wait_for_frame(&frames, after, Duration::from_secs(40)).await;
        let storage = tempfile::tempdir().expect("their storage");
        let them = attach(AttachConfig {
            instance: daemon.instance.clone(),
            config_dir: Some(daemon.config_dir.path().to_path_buf()),
            storage_dir: storage.path().to_path_buf(),
            identity: fixed_identity(seed),
            display_name: name.as_bytes().to_vec(),
            notices: Terminal::new(storage.path().to_path_buf()),
        })
        .await
        .expect("a correspondent attaches");
        attached.push(them);
        storages.push(storage);
    }

    let output = child.await.expect("the loquax child thread");
    for them in attached {
        them.stop().await;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {stderr}\nframes:\n{got}"
    );
    // 1452: a name is the name alone until another peer shares it.
    let named = frame(&got, "named");
    assert!(!en || named.contains("Bob──"), "{named}");
    assert!(!en || named.contains("to Bob──"), "{named}");
    assert!(!en || !named.contains(" ·61dd"), "{named}");
    let collision = frame(&got, "collision");
    assert!(
        !en || collision.contains("Two contacts are called Bob"),
        "{collision}"
    );
    // The tail on both list rows, the header and the compose box; no mark.
    assert!(!en || collision.contains("Bob ·61dd"), "{collision}");
    assert!(!en || collision.contains("B0b ·"), "{collision}");
    assert!(!en || collision.contains("to Bob ·61dd"), "{collision}");
    assert!(!en || !collision.contains('≈'), "{collision}");
    let petname = frame(&got, "petname");
    assert!(
        !en || petname.contains("Carol  known · announces Caroline"),
        "{petname}"
    );
    // The palette's rename asks with the petname to edit, and its answer
    // is the header's name; block takes Carol off the list, and the
    // `blocked` view lists her.
    let rename = frame(&got, "rename");
    assert!(!en || rename.contains("> Carol Meyer"), "{rename}");
    assert!(!en || rename.contains("your name for Carol "), "{rename}");
    let renamed = frame(&got, "renamed");
    assert!(
        !en || renamed.contains("Carol Meyer  known · announces Caroline"),
        "{renamed}"
    );
    let blocked = frame(&got, "blocked");
    assert!(
        !en || blocked
            .lines()
            .all(|row| !row.starts_with("│ Carol") && !row.starts_with("│▌Carol")),
        "{blocked}"
    );
    assert!(!en || blocked.contains("blocked Carol Meyer"), "{blocked}");
    let view = frame(&got, "blocked view");
    assert!(
        !en || view.contains("Blocked") && view.contains("Carol Meyer"),
        "{view}"
    );
    assert!(
        !en || view.contains("blocked · announces Caroline"),
        "{view}"
    );
    // The file says it, its comment kept.
    let contacts = std::fs::read_to_string(home.path().join("contacts")).expect("the contacts");
    assert_eq!(
        contacts,
        format!(
            "# the reader's own names\nname {carol} Carol Meyer\nblock {carol}\n",
            carol = hex(0x44)
        )
    );
    got
}

/// The first run: the exchange, then a send to Carol, who is never there,
/// and quit while the runner still waits for her.
const HISTORY_FIRST: &str = "\
until ready 20
frame attached
key enter
key i
type are you at the field site?
key enter
until delivery arrived 30
frame delivered
until received 1 30
key esc
key alt-down
key i
type leaving in ten
key enter
until leg direct 10
frame cut off
key esc
key q
";

/// The restart, with no `--to`: what the list holds is the store's.
const HISTORY_SECOND: &str = "\
until ready 20
frame restarted
key enter
key enter
frame ledger
key esc
key esc
key alt-down
frame bob
key :
type pin
key enter
frame pinned
key tab
key d
frame asked
key y
frame deleted
key q
";

const HISTORY_THIRD: &str = "\
until ready 20
frame again
key q
";

/// One run of loquax over `home`, `--to` each of `to`.
async fn history_run(
    daemon: &support::Daemon,
    home: &Path,
    name: &str,
    script: &str,
    to: &[u8],
) -> tokio::task::JoinHandle<std::process::Output> {
    let script_path = home.join(format!("{name}.script"));
    let frames = home.join(format!("{name}.frames"));
    std::fs::write(&script_path, script).expect("write the script");
    let _ = std::fs::remove_file(&frames);
    let mut args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
    ];
    for &seed in to {
        let address = delivery_address(&fixed_identity(seed)).expect("an address");
        args.extend([
            "--to".into(),
            address.iter().map(|b| format!("{b:02x}")).collect(),
        ]);
    }
    args.extend([
        "--script".into(),
        script_path.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x24".into(),
    ]);
    let child_home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    })
}

/// A run's frames, after it ended well.
async fn history_frames(
    run: tokio::task::JoinHandle<std::process::Output>,
    home: &Path,
    name: &str,
) -> String {
    let output = run.await.expect("the loquax child thread");
    let got = std::fs::read_to_string(home.join(format!("{name}.frames"))).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{name}: stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    got
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_shows_the_earlier_exchange_with_its_marks() {
    same_session(
        "tui_history.frames",
        &a_restart_shows_the_earlier_exchange_with_its_marks_in("").await,
    );
}

/// [`a_restart_shows_the_earlier_exchange_with_its_marks`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_shows_the_earlier_exchange_with_its_marks_in_german() {
    let frames = a_restart_shows_the_earlier_exchange_with_its_marks_in("lang de\n").await;
    assert!(frames.contains("du: "), "{frames}");
    same_session("tui_history_de.frames", &frames);
}

/// The session of [`a_restart_shows_the_earlier_exchange_with_its_marks`] with `lang` before every script: its frames.
async fn a_restart_shows_the_earlier_exchange_with_its_marks_in(lang: &str) -> String {
    let en = lang.is_empty();
    let daemon = daemon("tui-history", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    let our_address = delivery_address(&fixed_identity(0x11)).expect("our address");

    // The first run, with Bob there for the exchange.
    let first = history_run(
        &daemon,
        home.path(),
        "first",
        &format!("{lang}{HISTORY_FIRST}"),
        &[0x22, 0x33],
    )
    .await;
    wait_for_frame(
        &home.path().join("first.frames"),
        "attached",
        Duration::from_secs(20),
    )
    .await;
    let storage = tempfile::tempdir().expect("bob's storage");
    let mut bob = attach(AttachConfig {
        instance: daemon.instance.clone(),
        config_dir: Some(daemon.config_dir.path().to_path_buf()),
        storage_dir: storage.path().to_path_buf(),
        identity: fixed_identity(0x22),
        display_name: b"Bob".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("Bob attaches");
    wait_for_frame(
        &home.path().join("first.frames"),
        "delivered",
        Duration::from_secs(40),
    )
    .await;
    let reply = run_send(
        &mut bob.outbox,
        SendRequest {
            destination: our_address,
            title: Vec::new(),
            body: b"yes, from ten".to_vec(),
            fields: Vec::new(),
            via: Via::Direct,
        },
        &SendOptions::new(daemon.instance.clone(), Duration::from_secs(20)),
        &Tokio::new(),
    )
    .await;
    assert!(
        !en || reply.is_ok(),
        "Bob's reply did not go out: {reply:?}"
    );
    let first = history_frames(first, home.path(), "first").await;
    bob.stop().await;
    let cut = frame(&first, "cut off");
    assert!(!en || cut.contains("leaving in ten"), "{cut}");

    // The restart: no `--to`, and nobody else attached.
    let second = history_run(
        &daemon,
        home.path(),
        "second",
        &format!("{lang}{HISTORY_SECOND}"),
        &[],
    )
    .await;
    let second = history_frames(second, home.path(), "second").await;
    let restarted = frame(&second, "restarted");
    assert!(!en || restarted.contains("leaving in ten"), "{restarted}");
    assert!(
        !en || restarted.contains(" ◷"),
        "the cut-off send is queued: {restarted}"
    );
    assert!(
        !en || restarted.contains("Bob") && !restarted.contains("Bob ·61dd"),
        "Bob's name from the first run: {restarted}"
    );
    assert!(
        !en || frame(&second, "ledger").contains("Loquax restarted: no longer being sent"),
        "{second}"
    );
    let bob_frame = frame(&second, "bob");
    assert!(
        !en || bob_frame.contains("are you at the field site?"),
        "{bob_frame}"
    );
    // A proof is one tick (1462), and it survives the restart.
    assert!(
        !en || bob_frame.contains("field site?  12:00 ✓ "),
        "the proof survived: {bob_frame}"
    );
    assert!(!en || bob_frame.contains("yes, from ten"), "{bob_frame}");
    assert!(!en || frame(&second, "pinned").contains('┄'), "{second}");
    assert!(
        !en || frame(&second, "asked").contains("y deletes"),
        "{second}"
    );
    assert!(
        !en || !frame(&second, "deleted").contains("yes, from ten"),
        "{second}"
    );

    // A third start: the pin and the delete were written.
    let third = history_run(
        &daemon,
        home.path(),
        "third",
        &format!("{lang}{HISTORY_THIRD}"),
        &[],
    )
    .await;
    let third = history_frames(third, home.path(), "third").await;
    let again = frame(&third, "again");
    assert!(!en || again.contains('┄'), "{again}");
    assert!(!en || !again.contains("yes, from ten"), "{again}");

    let all = format!("=== first ===\n{first}=== second ===\n{second}=== third ===\n{third}");
    all
}

/// The draft's first run: a line typed to Bob, then on to Carol.
const DRAFT_FIRST: &str = "\
until ready 20
frame attached
key enter
key i
type half a thought
frame typed
key esc
key alt-down
frame left
key alt-up
frame back
key alt-down
key q
";

/// The restart, with no `--to`.
const DRAFT_SECOND: &str = "\
until ready 20
frame restarted
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn a_draft_stays_when_the_reader_goes_on_and_after_a_restart() {
    same_session(
        "tui_draft.frames",
        &a_draft_stays_when_the_reader_goes_on_and_after_a_restart_in("").await,
    );
}

/// [`a_draft_stays_when_the_reader_goes_on_and_after_a_restart`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_stays_when_the_reader_goes_on_and_after_a_restart_in_german() {
    let frames = a_draft_stays_when_the_reader_goes_on_and_after_a_restart_in("lang de\n").await;
    assert!(frames.contains("Entwurf"), "{frames}");
    same_session("tui_draft_de.frames", &frames);
}

/// The session of [`a_draft_stays_when_the_reader_goes_on_and_after_a_restart`] with `lang` before every script: its frames.
async fn a_draft_stays_when_the_reader_goes_on_and_after_a_restart_in(lang: &str) -> String {
    let en = lang.is_empty();
    let daemon = daemon("tui-draft", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");

    let first = history_run(
        &daemon,
        home.path(),
        "first",
        &format!("{lang}{DRAFT_FIRST}"),
        &[0x22, 0x33],
    )
    .await;
    let first = history_frames(first, home.path(), "first").await;
    let typed = frame(&first, "typed");
    assert!(
        !en || !typed.contains("Draft:"),
        "the open one's row: {typed}"
    );
    let left = frame(&first, "left");
    assert!(!en || left.contains("Draft: half a thought"), "{left}");
    let back = frame(&first, "back");
    assert!(
        !en || back.contains("half a thought"),
        "compose holds it: {back}"
    );
    assert!(!en || !back.contains("Draft:"), "{back}");

    // The restart: no `--to`, the store's conversations only.
    let second = history_run(
        &daemon,
        home.path(),
        "second",
        &format!("{lang}{DRAFT_SECOND}"),
        &[],
    )
    .await;
    let second = history_frames(second, home.path(), "second").await;
    let restarted = frame(&second, "restarted");
    assert!(!en || restarted.contains("half a thought"), "{restarted}");
    assert!(
        !en || !restarted.contains("Draft:"),
        "open, in compose: {restarted}"
    );

    let all = format!("=== first ===\n{first}=== second ===\n{second}");
    all
}

/// The telemetry session: every time on screen is the script's, and the
/// fixes the peer sends are stamped against that same clock, so the ages
/// are the same on every run.
const TELEMETRY_SCRIPT: &str = "\
clock 2026-10-26 14:00
until ready 20
frame attached
until received 3 60
tick 12
frame folded
key enter
key I
frame detail
key esc
key q
";

/// 2026-10-26 14:00 UTC, the script's clock.
const TELEMETRY_NOW: i64 = 1_793_023_200;

/// One position report as the peer's Telemeter packs it.
fn position_report(minutes_ago: i64, lat_e6: i32) -> Vec<(i64, Vec<u8>)> {
    use leviculum_lxmf::telemetry::{Location, Telemetry};
    let at = TELEMETRY_NOW - minutes_ago * 60;
    let telemetry = Telemetry {
        time: Some(at),
        location: Some(Location {
            latitude_e6: lat_e6,
            longitude_e6: 11_256_000,
            altitude_e2: 5_120,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 500,
            last_update: at,
        }),
        ..Telemetry::default()
    };
    vec![(
        leviculum_lxmf::constants::FIELD_TELEMETRY,
        telemetry.encode_field_value(),
    )]
}

async fn telemetry_session(size: &str, script: &str) -> String {
    let messages = |_: [u8; 16]| {
        vec![
            ("leaving the hut now", Vec::new()),
            ("", position_report(10, 43_770_000)),
            ("", position_report(4, 43_773_100)),
        ]
    };
    peer_session("telemetry", size, script, &[], |_| {}, messages)
        .await
        .0
}

/// A scripted session against a fresh daemon in which one peer, `0x22`,
/// sends us `messages(our_address)` directly once the frame `attached` is
/// written: the bodies and fields of each. `prepare` fills the home before
/// loquax starts; `extra` are more arguments. The frames, ids masked, and
/// the session's `tui.log`.
async fn peer_session(
    name: &str,
    size: &str,
    script_text: &str,
    extra: &[&str],
    prepare: impl FnOnce(&Path),
    messages: impl FnOnce([u8; 16]) -> Vec<(&'static str, Vec<(i64, Vec<u8>)>)>,
) -> (String, String) {
    let (frames, log, _) =
        peer_session_of(name, size, script_text, extra, prepare, messages, false).await;
    (frames, log)
}

/// [`peer_session`]; with `listen`, the peer also reads what loquax sends
/// it until loquax quits, and returns each message's body and fields.
async fn peer_session_of(
    name: &str,
    size: &str,
    script_text: &str,
    extra: &[&str],
    prepare: impl FnOnce(&Path),
    messages: impl FnOnce([u8; 16]) -> Vec<(&'static str, Vec<(i64, Vec<u8>)>)>,
    listen: bool,
) -> (String, String, Vec<(Vec<u8>, Vec<(i64, Vec<u8>)>)>) {
    let daemon = daemon(&format!("tui-{name}-{size}"), |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    prepare(home.path());
    // No session reaches the network (1089): the map's autoload is off
    // unless the session's own config names a source on the loopback.
    if !home.path().join("config").exists() {
        std::fs::write(home.path().join("config"), "map_autoload = off\n").expect("config");
    }
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let our_address = delivery_address(&ours).expect("our address");
    let theirs = fixed_identity(0x22);
    let their_hex: String = delivery_address(&theirs)
        .expect("their address")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let script = home.path().join(format!("{name}.script"));
    let frames = home.path().join(format!("{name}.frames"));
    std::fs::write(&script, script_text).expect("write the script");
    let mut args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        their_hex,
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        size.into(),
    ];
    args.extend(extra.iter().map(|a| a.to_string()));
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    wait_for_frame(&frames, "attached", Duration::from_secs(20)).await;
    let storage = tempfile::tempdir().expect("their storage");
    let mut them = attach(AttachConfig {
        instance: daemon.instance.clone(),
        config_dir: Some(daemon.config_dir.path().to_path_buf()),
        storage_dir: storage.path().to_path_buf(),
        identity: theirs,
        display_name: b"them".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("the correspondent attaches");
    let options = SendOptions::new(daemon.instance.clone(), Duration::from_secs(20));
    let mut fanout = Fanout::default();
    for (body, fields) in messages(our_address) {
        let mut tap = fanout.tap();
        let clock = Tokio::new();
        let send = run_send(
            &mut tap,
            SendRequest {
                destination: our_address,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields,
                via: Via::Direct,
            },
            &options,
            &clock,
        );
        tokio::pin!(send);
        let sent = loop {
            tokio::select! {
                sent = &mut send => break sent,
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    let _ = fanout.pump(&mut them.outbox);
                }
            }
        };
        assert!(sent.is_ok(), "{body:?} did not go out: {sent:?}");
    }

    // The peer listens until loquax quits.
    let mut heard = Vec::new();
    let mut child = child;
    let output = loop {
        if !listen {
            break (&mut child).await.expect("the loquax child thread");
        }
        tokio::select! {
            output = &mut child => break output.expect("the loquax child thread"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {
                for event in fanout.pump(&mut them.outbox).unwrap_or_default() {
                    if let OutboxEvent::Received { body, fields, .. } = event {
                        heard.push((body, fields));
                    }
                }
            }
        }
    };
    them.stop().await;
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{size}: stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    (without_ids(&got), log, heard)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peers_position_shows_in_the_header_and_its_reports_fold_into_one_line() {
    same_session(
        "tui_telemetry.frames",
        &a_peers_position_shows_in_the_header_and_its_reports_fold_into_one_line_in("").await,
    );
}

/// [`a_peers_position_shows_in_the_header_and_its_reports_fold_into_one_line`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_peers_position_shows_in_the_header_and_its_reports_fold_into_one_line_in_german() {
    let frames =
        a_peers_position_shows_in_the_header_and_its_reports_fold_into_one_line_in("lang de\n")
            .await;
    assert!(frames.contains("Live-Position"), "{frames}");
    same_session("tui_telemetry_de.frames", &frames);
}

/// The session of [`a_peers_position_shows_in_the_header_and_its_reports_fold_into_one_line`] with `lang` before every script: its frames.
async fn a_peers_position_shows_in_the_header_and_its_reports_fold_into_one_line_in(
    lang: &str,
) -> String {
    let en = lang.is_empty();
    let mut all = String::new();
    for size in ["80x24", "120x40"] {
        let frames = telemetry_session(size, &format!("{lang}{TELEMETRY_SCRIPT}")).await;
        let folded = frame(&frames, "folded");
        // One line, one sentence (1449 item 5): the coordinates are the
        // Info's.
        assert_eq!(
            folded
                .matches(if en {
                    "◎ Live position"
                } else {
                    "◎ Live-Position"
                })
                .count(),
            1,
            "{size}: the two reports are one line\n{folded}"
        );
        assert!(
            !en || !folded.contains("◎ position 43"),
            "{size}: no coordinates in the bubble\n{folded}"
        );
        match size {
            "80x24" => assert!(!en || folded.contains("pos 4 min"), "{folded}"),
            _ => assert!(
                !en || folded.contains("at 43.77310 N 11.25600 E · ±5 m · 4 min ago"),
                "{folded}"
            ),
        }
        let detail = frame(&frames, "detail");
        assert!(
            !en || detail.contains("position  43.77310 N 11.25600 E"),
            "{size}: {detail}"
        );
        assert!(
            !en || detail.contains("fix       2026-10-26 13:56:00"),
            "{detail}"
        );
        all.push_str(&format!("=== {size} ===\n{frames}"));
    }
    all
}

/// Read receipts, slice two (1231): the peer's three messages carry the
/// `lev` mark; the terminal's focus comes in once they are on screen. A
/// second later they are seen, five seconds after that one receipt goes,
/// alone, with the three ids and the script's clock as the read time, and
/// the peer hears it. No bubble is drawn for it.
const RECEIPTS_SCRIPT: &str = "\
until ready 20
frame attached
until received 3 60
focus in
sleep 1500
frame seen
sleep 9000
frame after
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn the_terminal_sends_one_receipt_six_seconds_after_the_focus_came() {
    // The host's load over the run (1473, 1489): the receipt's lateness
    // is judged only on a quiet one.
    let host = loquax::host_load::HostLoad::start();
    let messages = |_: [u8; 16]| {
        let mark = leviculum_lxmf::lev::Lev::mark().fields().to_vec();
        vec![
            ("first", mark.clone()),
            ("second", mark.clone()),
            ("third", mark),
        ]
    };
    let (frames, log, heard) = peer_session_of(
        "receipts",
        "80x24",
        RECEIPTS_SCRIPT,
        &[],
        |_| {},
        messages,
        true,
    )
    .await;
    let elapsed = |prefix: &str| -> Vec<u64> {
        log.lines()
            .filter(|line| line.contains(prefix))
            .filter_map(|line| line.split("elapsed_ms=").nth(1))
            .filter_map(|ms| ms.trim().parse().ok())
            .collect()
    };
    let focus = elapsed("FOCUS front=true");
    let sent = elapsed("RECEIPT send");
    assert_eq!(focus.len(), 1, "{log}");
    assert_eq!(sent.len(), 1, "one receipt for the visit\n{log}");
    let took = sent[0] - focus[0];
    // Seen one second after the focus, sent five after that: never
    // sooner. The script pumps every 20 ms; how much later a loaded host
    // makes it is the host's, so that bar counts only on a quiet one
    // (finding 14 of the 2026-11-07 review, 1489).
    assert!(took >= 6_000, "{took} ms\n{log}");
    if host.measured("the receipt's lateness") {
        assert!(took < 7_000, "{took} ms\n{log}");
    }
    let line = log
        .lines()
        .find(|line| line.contains("RECEIPT send"))
        .expect("the line");
    assert!(
        line.contains("ids=3") && line.contains("alone=true"),
        "{line}"
    );
    let receipts: Vec<_> = heard
        .iter()
        .filter_map(|(body, fields)| {
            leviculum_lxmf::lev::Lev::from_fields(fields)
                .and_then(|lev| lev.receipt)
                .map(|r| (body.clone(), r))
        })
        .collect();
    assert_eq!(receipts.len(), 1, "the peer hears one: {heard:?}");
    let (body, receipt) = &receipts[0];
    assert!(body.is_empty(), "a receipt alone has no text");
    assert_eq!(receipt.ids.len(), 3);
    // The script's clock: 2026-10-26 12:00 UTC.
    assert_eq!(receipt.read_at, 1_793_016_000);
    // The list's row and the three bubbles carry the time; a bubble for
    // the receipt would be a fifth.
    let after = frame(&frames, "after");
    assert_eq!(
        after.matches("12:00").count(),
        4,
        "no bubble for it\n{after}"
    );
    assert!(after.contains("third  12:00"), "{after}");
    eprintln!("RECEIPT left {took} ms after the focus came");
}

/// A position report is a state, not a message (1209): six reports ten
/// minutes apart are one line that says the position is live, every 10
/// min, with no badge in the list; 25 minutes later it is recent (`last
/// position 25 min ago`), 25 hours later stale.
const LIVENESS_SCRIPT: &str = "\
clock 2026-10-26 14:00
until ready 20
frame attached
until received 6 60
tick 12
frame live
clock 2026-10-26 14:25
tick 12
frame recent
clock 2026-10-27 15:00
tick 12
frame stale
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn a_position_reports_line_says_live_recent_or_stale_and_no_badge() {
    same_session(
        "tui_liveness.frames",
        &a_position_reports_line_says_live_recent_or_stale_and_no_badge_in("").await,
    );
}

/// [`a_position_reports_line_says_live_recent_or_stale_and_no_badge`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn a_position_reports_line_says_live_recent_or_stale_and_no_badge_in_german() {
    let frames =
        a_position_reports_line_says_live_recent_or_stale_and_no_badge_in("lang de\n").await;
    assert!(frames.contains("Position"), "{frames}");
    same_session("tui_liveness_de.frames", &frames);
}

/// The session of [`a_position_reports_line_says_live_recent_or_stale_and_no_badge`] with `lang` before every script: its frames.
async fn a_position_reports_line_says_live_recent_or_stale_and_no_badge_in(lang: &str) -> String {
    let en = lang.is_empty();
    let messages = |_: [u8; 16]| {
        (0..6)
            .map(|i| {
                (
                    "",
                    position_report(50 - i * 10, 43_770_000 + i as i32 * 100),
                )
            })
            .collect()
    };
    let frames = peer_session(
        "liveness",
        "80x24",
        &format!("{lang}{LIVENESS_SCRIPT}"),
        &[],
        |_| {},
        messages,
    )
    .await
    .0;
    let live = frame(&frames, "live");
    let joined = |f: &str| f.split_whitespace().collect::<Vec<_>>().join(" ");
    // The bubble says one sentence (1449 item 5); the list says how alive.
    assert!(!en || joined(live).contains("◎ Live position"), "{live}");
    assert!(!en || live.contains("◎ position · live"), "{live}");
    assert!(!en || live.contains("⌖"), "the map pin\n{live}");
    assert!(
        !en || !live.contains("[1]") && !live.contains("[6]"),
        "no badge\n{live}"
    );
    let recent = frame(&frames, "recent");
    assert!(
        !en || joined(recent).contains("◎ Position from today"),
        "{recent}"
    );
    // The entry's map pin (1224) stands left of where a badge would.
    assert!(!en || recent.contains("◎ position · 25 mi… ⌖"), "{recent}");
    let stale = frame(&frames, "stale");
    assert!(
        !en || joined(stale).contains("◎ Position from yesterday"),
        "{stale}"
    );
    assert!(!en || !stale.contains("⌖"), "no pin past a day\n{stale}");
    frames
}

/// The map session (`docs/src/concepts/loquax-map.md`, slice (c)): the
/// peer reports its own position and, as a collector, a stream row for a
/// third peer three hours old and one for us. With the Florence fixture
/// installed as the only region, `M` fits the three pins on it; `Tab`
/// moves the selection; a drag that starts on the selected pin moves the
/// map ten cells left and three up under the pointer and opens nothing;
/// zooming out past the region's shallowest zoom
/// to z5 draws the world outline, the coast of Italy and San Marino's
/// border; `0` fits again; two `Tab`s, past our own pin, bring the
/// selection back to the peer, which sorts before "Unknown" by name since
/// 1452 (it sorted after the hex form before); `Enter` opens the selected
/// peer's conversation; the map again and `Esc` bring the conversation
/// back.
const MAP_SCRIPT: &str = "\
clock 2026-10-26 14:00
until ready 20
frame attached
until received 1 60
tick 12
key M
frame map
key tab
frame next pin
drag 50 11 40 8
frame dragged
key -
key -
key -
key -
key -
key -
key -
key -
key -
frame outline
key 0
key tab
key tab
key enter
frame opened
key M
key esc
frame back
key enter
frame from the report
key esc
frame report closed
key q
";

/// The third peer the stream reports.
const CAROL: [u8; 16] = [0x33; 16];

fn located(minutes_ago: i64, lat_e6: i32, lon_e6: i32) -> leviculum_lxmf::telemetry::Telemetry {
    use leviculum_lxmf::telemetry::{Location, Telemetry};
    let at = TELEMETRY_NOW - minutes_ago * 60;
    Telemetry {
        time: Some(at),
        location: Some(Location {
            latitude_e6: lat_e6,
            longitude_e6: lon_e6,
            altitude_e2: 5_120,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 500,
            last_update: at,
        }),
        ..Telemetry::default()
    }
}

async fn map_session(graphics: &str, script: &str) -> (String, String) {
    map_session_of("map", script, graphics).await
}

/// The map session's peers and region, `script` played with `--graphics`.
async fn map_session_of(name: &str, script: &str, graphics: &str) -> (String, String) {
    let install = |home: &Path| {
        let maps = home.join("maps");
        std::fs::create_dir_all(&maps).expect("maps dir");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../leviculum-map/fixtures/firenze.lmr"
            ),
            maps.join("firenze.lmr"),
        )
        .expect("install the fixture region");
        // Carol's fix and our own, kept from before the session. Until the
        // audit of 1787 they came as stream rows in the peer's message,
        // which Loquax now files under nobody: the peer is no collector
        // the reader chose, and one of the rows named the reader itself.
        let ours = delivery_address(&fixed_identity(0x11)).expect("our address");
        let theirs = delivery_address(&fixed_identity(0x22)).expect("their address");
        let kept = |peer: [u8; 16], telemetry: leviculum_lxmf::telemetry::Telemetry| {
            loquax::ui::history::StoreWrite::Position(loquax::telemetry::Fix {
                peer,
                via: theirs,
                message: None,
                received_at: Some(TELEMETRY_NOW),
                location: telemetry.location.expect("a location"),
                telemetry: telemetry.encode(),
                source: loquax::telemetry::Source::Telemetry,
            })
        };
        let mut store =
            loquax::store::Store::open(&home.join(loquax::store::STORE_FILE), ours).expect("store");
        store
            .write(&[
                kept(CAROL, located(180, 43_776_000, 11_266_000)),
                kept(ours, located(1, 43_774_000, 11_258_000)),
            ])
            .expect("keep the fixes");
    };
    let messages = |_ours: [u8; 16]| {
        let own = vec![(
            leviculum_lxmf::constants::FIELD_TELEMETRY,
            located(4, 43_775_000, 11_261_000).encode_field_value(),
        )];
        vec![("", own)]
    };
    let flag = format!("--graphics={graphics}");
    peer_session(name, "80x24", script, &[&flag], install, messages).await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks() {
    same_session(
        "tui_map.frames",
        &the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks_in("")
            .await,
    );
}

/// [`the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks_in_german()
{
    let frames = the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks_in(
        "lang de\n",
    )
    .await;
    assert!(frames.contains("vor "), "{frames}");
    same_session("tui_map_de.frames", &frames);
}

/// The session of [`the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks`] with `lang` before every script: its frames.
async fn the_map_shows_the_pins_on_the_region_and_the_outline_in_braille_and_halfblocks_in(
    lang: &str,
) -> String {
    let en = lang.is_empty();
    let mut all = String::new();
    for graphics in ["braille", "halfblocks"] {
        let (frames, log) = map_session(graphics, &format!("{lang}{MAP_SCRIPT}")).await;
        let line = log
            .lines()
            .find_map(|l| l.find("GRAPHICS ").map(|at| &l[at..]))
            .unwrap_or_default();
        assert_eq!(
            line,
            format!(
                "GRAPHICS backend={graphics} reason=forced detected=none between=none cell={} zooms=0-20 \
                 context=local:unknown",
                match graphics {
                    "braille" => "2x4",
                    _ => "1x2",
                }
            ),
            "{log}"
        );
        let map = frame(&frames, "map");
        assert!(
            !en || map.contains("● map"),
            "the map has the pane and the focus\n{map}"
        );
        assert!(!en || map.contains("© OpenStreetMap contributors"), "{map}");
        assert_eq!(
            map.matches('●').count(),
            3,
            "the title's mark and two pins\n{map}"
        );
        assert_eq!(map.matches('◉').count(), 1, "the selected pin\n{map}");
        // Our own position is the dot alone, no name and no age (1449
        // item 2): the control is that "you" was drawn before.
        assert!(!en || !map.contains("you · 1 min"), "{map}");
        // Tab: the selection moves from the first pin by name to the next.
        let next = frame(&frames, "next pin");
        assert!(
            // By name, "them" before "Unknown" (1452: no hex to sort first).
            !en || map.contains("◉ them") && next.contains("· 3 h ago ◉"),
            "{map}\n{next}"
        );
        assert!(
            !en || map.contains("· 3 h"),
            "the stale one with its age\n{map}"
        );
        // The drag: every pin ten cells left and three up, the map open.
        let dragged = frame(&frames, "dragged");
        assert!(
            !en || dragged.contains("● map"),
            "nothing opened\n{dragged}"
        );
        let at = |frame: &str| {
            frame.lines().enumerate().find_map(|(row, line)| {
                line.find('◉')
                    .map(|byte| (line[..byte].chars().count(), row))
            })
        };
        let (Some(was), Some(now)) = (at(next), at(dragged)) else {
            panic!("the selected pin in both\n{next}\n{dragged}");
        };
        assert_eq!((was.0 - now.0, was.1 - now.1), (10, 3), "{dragged}");
        assert!(!en || map.contains("· 4 min"), "{map}");
        let outline = frame(&frames, "outline");
        assert!(!en || outline.contains("Natural Earth"), "{outline}");
        let opened = frame(&frames, "opened");
        assert!(
            !en || !opened.contains("● map"),
            "the conversation is back\n{opened}"
        );
        let back = frame(&frames, "back");
        assert!(
            !en || !back.contains("Natural Earth") && !back.contains("OpenStreetMap"),
            "{back}"
        );
        // Enter on the report: the map at its fix, the region's deepest
        // zoom, the sender's pin selected; Esc brings the conversation back.
        let reported = frame(&frames, "from the report");
        assert!(!en || reported.contains("● map"), "{reported}");
        // No zoom number on screen (1447): the debug overlay alone has it.
        assert!(!en || !reported.contains("z15"), "{reported}");
        assert!(!en || reported.contains("◉ them"), "{reported}");
        assert_eq!(frame(&frames, "report closed"), back);
        all.push_str(&format!("=== {graphics} ===\n{frames}"));
    }
    all
}

/// The map's drawing cycled live (1093): opened in octants, `t` steps
/// through the text techniques in the ladder's order, the title naming each,
/// `T` steps back, and `g` puts the glyph test over the map. A script
/// writes no config and reads none, so its frames do not depend on a run
/// before.
const MAP_CYCLE_SCRIPT: &str = "\
clock 2026-10-26 14:00
until ready 20
frame attached
until received 1 60
tick 12
key M
frame octant
key t
frame blocks
key t
frame halfblocks
key t
frame braille-thin
key t
frame mosaic
key t
frame braille
key T
frame back to mosaic
tick 8
frame the name gone
key g
frame glyph test
key g
frame glyph test closed
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn the_map_drawing_cycles_through_the_text_techniques() {
    same_session(
        "tui_map_cycle.frames",
        &the_map_drawing_cycles_through_the_text_techniques_in("").await,
    );
}

/// [`the_map_drawing_cycles_through_the_text_techniques`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn the_map_drawing_cycles_through_the_text_techniques_in_german() {
    let frames = the_map_drawing_cycles_through_the_text_techniques_in("lang de\n").await;
    assert!(frames.contains("vor "), "{frames}");
    same_session("tui_map_cycle_de.frames", &frames);
}

/// The session of [`the_map_drawing_cycles_through_the_text_techniques`] with `lang` before every script: its frames.
async fn the_map_drawing_cycles_through_the_text_techniques_in(lang: &str) -> String {
    let en = lang.is_empty();
    let (frames, log) =
        map_session_of("mapcycle", &format!("{lang}{MAP_CYCLE_SCRIPT}"), "octant").await;
    assert!(
        !en || log.contains(
            "GRAPHICS backend=octant reason=forced detected=none between=none cell=2x4 zooms=0-20 \
             context=local:unknown"
        ),
        "{log}"
    );
    // The picture's rows: inside the map pane's border, above the credit.
    let picture = |label: &str| -> String {
        frame(&frames, label)
            .lines()
            .skip(1)
            .take(18)
            .map(|l| l.chars().skip(27).take(52).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let octants = |p: &str| {
        p.chars()
            .filter(|c| ('\u{1CD00}'..='\u{1CDE5}').contains(c))
            .count()
    };
    let braille = |p: &str| {
        p.chars()
            .filter(|c| ('\u{2801}'..='\u{28FF}').contains(c))
            .count()
    };
    let octant = picture("octant");
    assert!(!en || octants(&octant) > 50, "{octant}");
    let blocks = picture("blocks");
    assert_eq!(octants(&blocks), 0, "{blocks}");
    assert!(
        !en || blocks
            .chars()
            .any(|c| "▘▝▀▖▌▞▛▗▚▐▜▄▙▟▁▂▃▅▆▇▔▎▊".contains(c)),
        "{blocks}"
    );
    let half = picture("halfblocks");
    assert!(
        !en || half.chars().filter(|&c| c == '▀').count() > 400,
        "{half}"
    );
    let thin = picture("braille-thin");
    assert!(!en || braille(&thin) > 50 && octants(&thin) == 0, "{thin}");
    let mosaic = picture("mosaic");
    assert_eq!(braille(&mosaic) + octants(&mosaic), 0, "{mosaic}");
    let plain = picture("braille");
    assert!(!en || braille(&plain) > 50, "{plain}");
    // The title names each new drawing, until the ticks pass.
    for (label, name) in [
        ("blocks", "blocks"),
        ("halfblocks", "halfblocks"),
        ("braille-thin", "braille-thin"),
        ("mosaic", "mosaic"),
        ("braille", "braille"),
        ("back to mosaic", "mosaic"),
    ] {
        let top = frame(&frames, label).lines().next().unwrap_or_default();
        assert!(!en || top.contains(name), "{label}: {top}");
        assert!(!en || !top.contains("z14"), "no zoom number (1447): {top}");
    }
    let named = |top: &str| {
        ["octant", "blocks", "braille", "mosaic"]
            .iter()
            .any(|n| top.contains(n))
    };
    let top = frame(&frames, "octant").lines().next().unwrap_or_default();
    assert!(!en || !named(top), "named only after a change: {top}");
    let top = frame(&frames, "the name gone")
        .lines()
        .next()
        .unwrap_or_default();
    assert!(!en || !named(top) && !top.contains("z14"), "{top}");
    // Every frame keeps the pins and the credit.
    for label in ["octant", "blocks", "braille-thin", "mosaic", "braille"] {
        let shown = frame(&frames, label);
        assert_eq!(shown.matches('●').count(), 3, "{label}\n{shown}");
        assert!(
            !en || shown.contains("© OpenStreetMap contributors"),
            "{label}\n{shown}"
        );
    }
    let test = frame(&frames, "glyph test");
    assert!(
        !en || test.contains("glyph test · g closes") && test.contains("octant"),
        "{test}"
    );
    assert!(!en || !frame(&frames, "glyph test closed").contains("glyph test"));
    frames
}

/// The autoload session (1089), against the loopback server serving the
/// committed fixture `leviculum-mapcut/fixtures/oltrarno.pmtiles`, no maps
/// installed. The peer reports a position in the fixture's box; `L` opens
/// the map there over the outline and asks for the place's region, the
/// title saying so; once it landed the map is fitted to it at z15 and the
/// streets are drawn. `d` over the place's region says it is loaded
/// already; zoomed out to z9 the view is wider than the place's box, and
/// `d` loads it.
const MAPLOAD_SCRIPT: &str = "\
clock 2026-10-26 14:00
until ready 20
frame attached
until received 1 60
tick 12
key L
frame before the load
until map 30
frame after the load
key d
frame held already
tick 40
key -
key -
key -
key -
key -
key -
key d
frame loading the area
until map 30
frame the area loaded
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn opening_the_map_at_a_place_loads_its_region_and_the_next_frame_draws_it() {
    same_session(
        "tui_mapload.frames",
        &opening_the_map_at_a_place_loads_its_region_and_the_next_frame_draws_it_in("").await,
    );
}

/// [`opening_the_map_at_a_place_loads_its_region_and_the_next_frame_draws_it`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn opening_the_map_at_a_place_loads_its_region_and_the_next_frame_draws_it_in_german() {
    let frames =
        opening_the_map_at_a_place_loads_its_region_and_the_next_frame_draws_it_in("lang de\n")
            .await;
    assert!(frames.contains("vor "), "{frames}");
    same_session("tui_mapload_de.frames", &frames);
}

/// The session of [`opening_the_map_at_a_place_loads_its_region_and_the_next_frame_draws_it`] with `lang` before every script: its frames.
async fn opening_the_map_at_a_place_loads_its_region_and_the_next_frame_draws_it_in(
    lang: &str,
) -> String {
    let en = lang.is_empty();
    const FIXTURE: &[u8] = include_bytes!("../../leviculum-mapcut/fixtures/oltrarno.pmtiles");
    let serve = leviculum_mapcut::serve::Serve::start(vec![(
        "oltrarno.pmtiles".to_string(),
        FIXTURE.to_vec(),
    )])
    .expect("serve the fixture");
    let source = serve.url("oltrarno.pmtiles");
    let prepare = move |home: &Path| {
        std::fs::write(home.join("config"), format!("map_source = {source}\n")).expect("config");
    };
    let messages = |_: [u8; 16]| {
        vec![(
            "",
            vec![(
                leviculum_lxmf::constants::FIELD_TELEMETRY,
                located(4, 43_747_300, 11_272_000).encode_field_value(),
            )],
        )]
    };
    let (frames, log) = peer_session(
        "mapload",
        "80x24",
        &format!("{lang}{MAPLOAD_SCRIPT}"),
        &["--graphics=braille"],
        prepare,
        messages,
    )
    .await;
    let before = frame(&frames, "before the load");
    // The place's region is what the screen shows: the title says so in
    // words since 1527 (`MAP_LOADING_HERE`), not the older "loading map".
    assert!(
        !en || before.contains("loading the map of this area"),
        "{before}"
    );
    // No region holds the place yet: the radar meanwhile (1386), centred
    // on the one pin, which it says, at the fit's zoom the region keeps.
    assert!(
        !en || before.contains("Rings 50 m around them") && !before.contains("z15"),
        "the radar meanwhile\n{before}"
    );
    let after = frame(&frames, "after the load");
    assert!(
        !en || !after.contains("z15") && !after.contains("in view"),
        "no zoom number and no count (1447)\n{after}"
    );
    assert!(
        !en || after.contains("© OpenStreetMap contributors"),
        "{after}"
    );
    assert!(!en || !after.contains("loading map"), "{after}");
    assert_ne!(before, after);
    let held = frame(&frames, "held already");
    assert!(
        !en || held.contains("the map here is loaded already"),
        "{held}"
    );
    let loading = frame(&frames, "loading the area");
    assert!(
        !en || loading.contains("─ loading the map of this area") && !loading.contains("z9"),
        "{loading}"
    );
    let lines: Vec<&str> = log
        .lines()
        .filter_map(|l| l.find("MAPLOAD ").map(|at| &l[at..]))
        .collect();
    let count = |word: &str| lines.iter().filter(|l| l.starts_with(word)).count();
    // Since 1527 the terminal runs the scheduler too: besides the place's
    // region and the zoomed-out area it loads the screen's neighbours
    // (`MAPLOAD view ... here= ahead=`). Every load that starts lands, none
    // fails, and there are at least the two this script asks for.
    let (started, done, failed) = (
        count("MAPLOAD start"),
        count("MAPLOAD done"),
        count("MAPLOAD failed"),
    );
    assert!(
        !en || started >= 2 && started == done && failed == 0,
        "{lines:#?}"
    );
    assert!(
        !en || lines
            .iter()
            .any(|l| l.starts_with("MAPLOAD start") && l.contains("reason=place")),
        "{lines:#?}"
    );
    assert!(
        !en || lines
            .iter()
            .any(|l| l.starts_with("MAPLOAD start") && l.contains("reason=view")),
        "{lines:#?}"
    );
    // A region a load, each its own file under the autoload's prefix.
    let files: Vec<&str> = lines
        .iter()
        .filter(|l| l.starts_with("MAPLOAD done"))
        .filter_map(|l| l.split(" file=").nth(1)?.split(' ').next())
        .collect();
    assert_eq!(files.len(), done, "{lines:#?}");
    let distinct: std::collections::BTreeSet<&&str> = files.iter().collect();
    assert_eq!(distinct.len(), files.len(), "{files:?}");
    assert!(
        !en || files
            .iter()
            .all(|f| f.starts_with("auto-") && f.ends_with(".lmr")),
        "{files:?}"
    );
    frames
}

/// One `loquax` CLI run against `daemon` from `home`, with `stdin`.
async fn cli(
    daemon: &support::Daemon,
    home: &Path,
    args: &[&str],
    stdin: &[u8],
) -> std::process::Output {
    let mut all: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    all.extend([
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
    ]);
    let (home, stdin) = (home.to_path_buf(), stdin.to_vec());
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&all)
            .env("LNMSG_HOME", &home)
            .env("LEVICULUM_EVENT_LOG", home.join("events.log"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        if let Some(mut pipe) = child.stdin.take() {
            std::io::Write::write_all(&mut pipe, &stdin).expect("write stdin");
        }
        child.wait_with_output().expect("wait for loquax")
    })
    .await
    .expect("the loquax child thread")
}

/// Leave `body` at the node for `to`, as `loquax send --via propagated`
/// does: exit 3, the node holds it.
async fn deposit(daemon: &support::Daemon, home: &Path, to: &str, pn: &str, body: &str) {
    let args = [
        "send",
        to,
        "--via",
        "propagated",
        "--pn",
        pn,
        "--timeout",
        "20",
    ];
    let sent = cli(daemon, home, &args, body.as_bytes()).await;
    assert_eq!(
        sent.status.code(),
        Some(3),
        "the node must hold {body:?}: {}\nevents:\n{}",
        String::from_utf8_lossy(&sent.stderr),
        std::fs::read_to_string(home.join("events.log")).unwrap_or_default()
    );
}

/// The window the second message must arrive in, from the first one's
/// frame: well inside the thirty-second bound, which the start's sync set
/// running before the first frame, and above the four-second cadence plus a
/// deposit and a drain on a loaded host.
const ACTIVITY_SCRIPT: &str = "\
until ready 20
until received 1 30
frame first
until received 2 20
frame second
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn a_message_left_at_the_mailbox_arrives_without_a_key() {
    let daemon = daemon("sync", |b| b).await;
    // lnpnd at stamp cost 0, so a deposit needs no proof-of-work.
    let pn = lnpnd_node::PropagationNode::start(&daemon.instance, 0).await;
    let pn_hex = pn.hex();
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let our_hex: String = delivery_address(&ours)
        .expect("our address")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let sender = tempfile::tempdir().expect("sender home");

    // We, attached in this process for the first deposit: the sender needs
    // our announce to encrypt to us, and the daemon forgets the path of a
    // client that has gone, so a CLI run of ours that announced and left
    // would not do.
    // In the screen's own storage: the announce carries the delivery
    // destination's ratchet (1185), the sender encrypts the deposit to it,
    // and the screen opens it with the ratchets it loads from there. An
    // identity that announced from one storage and reads in another is the
    // identity moved without its ratchets.
    let storage = tempfile::tempdir().expect("our notices");
    let early = attach(AttachConfig {
        instance: daemon.instance.clone(),
        config_dir: Some(daemon.config_dir.path().to_path_buf()),
        storage_dir: home.path().join("storage"),
        identity: fixed_identity(0x11),
        display_name: b"tui".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("we attach");
    deposit(
        &daemon,
        sender.path(),
        &our_hex,
        &pn_hex,
        "waiting before the start",
    )
    .await;
    early.stop().await;

    let script = home.path().join("sync.script");
    let frames = home.path().join("sync.frames");
    std::fs::write(&script, ACTIVITY_SCRIPT).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--pn".into(),
        pn_hex.clone(),
        "--timeout".into(),
        "5".into(),
        "--sync-interval".into(),
        "30s".into(),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "120x24".into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    wait_for_frame(&frames, "first", Duration::from_secs(60)).await;
    deposit(
        &daemon,
        sender.path(),
        &our_hex,
        &pn_hex,
        "left while it runs",
    )
    .await;

    let output = child.await.expect("the loquax child thread");
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\nframes:\n{got}\ntui.log:\n{log}",
        String::from_utf8_lossy(&output.stderr)
    );
    let second = frame(&got, "second");
    assert!(
        second.contains("waiting before the start") && second.contains("left while it runs"),
        "{second}"
    );
    let syncs: Vec<&str> = log
        .lines()
        .filter_map(|line| line.split("SYNC reason=").nth(1))
        .collect();
    assert!(log.contains("SYNC bound=30s"), "{log}");
    assert_eq!(syncs.first(), Some(&"start"), "{log}");
    assert!(
        syncs[1..].iter().all(|r| *r == "timer"),
        "no key, no announce: {log}"
    );
    assert!(syncs.len() >= 2, "{log}");
    // Every sync's end is in the log, with what it brought (leviculum 1072).
    assert!(
        log.contains("SYNC done reason=start result=ok new=1 elapsed_ms="),
        "{log}"
    );
}

/// A propagation node that is announced and never answers a link: a bare
/// `lxmf.propagation` destination on a client of `daemon`, registered with
/// inbound links declined. A drain against it asks for the link until the
/// link's establishment timeout, as against a node five hops out whose link
/// proofs are lost. The hash, and what keeps it alive.
async fn silent_propagation_node(
    daemon: &support::Daemon,
) -> ([u8; 16], leviculum_std::ReticulumNode, tempfile::TempDir) {
    use leviculum_core::{Destination, DestinationHash, DestinationType, Direction};
    let storage = tempfile::tempdir().expect("silent pn storage");
    let mut node = leviculum_std::driver::ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&daemon.instance)
        .storage_path(storage.path().to_path_buf())
        .build_sync()
        .expect("build the silent pn client");
    node.start().await.expect("attach the silent pn client");
    let mut destination = Destination::new(
        Some(leviculum_std::generate_identity()),
        Direction::In,
        DestinationType::Single,
        "lxmf",
        &["propagation"],
    )
    .expect("propagation destination");
    destination.set_accepts_links(false);
    let hash: DestinationHash = *destination.hash();
    node.register_destination(destination);
    let app_data = leviculum_lxmf::propagation::PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 0,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 0,
        stamp_cost_flexibility: 0,
        peering_cost: 0,
        metadata: Vec::new(),
    }
    .encode()
    .expect("encode the announce");
    node.announce_destination(&hash, Some(&app_data))
        .await
        .expect("announce");
    (*hash.as_bytes(), node, storage)
}

/// A fetch against a node that never answers is running when a message is
/// sent: the send's direct leg must start at once, not when the fetch gives
/// up (leviculum 1072). `until leg direct 3` is the bound: the link to the
/// silent node takes its establishment timeout, over ten seconds, to fail.
const STALLED_FETCH_SCRIPT: &str = "\
until ready 20
key :
type fetch
key enter
key enter
key i
type while the mailbox stalls
key enter
until leg direct 3
frame sending
until delivery arrived 30
frame delivered
key esc
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn a_send_does_not_wait_behind_a_fetch_that_stalls() {
    let daemon = daemon("tui-stall", |b| b).await;
    let (silent, _silent_node, _silent_storage) = silent_propagation_node(&daemon).await;
    let silent_hex: String = silent.iter().map(|b| format!("{b:02x}")).collect();
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    // The ugly store (1173): nothing on screen is under test here.
    ugly_store::fill_store(home.path());
    let theirs = fixed_identity(0x22);
    let their_hex: String = delivery_address(&theirs)
        .expect("their address")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let script = home.path().join("stall.script");
    let frames = home.path().join("stall.frames");
    std::fs::write(&script, STALLED_FETCH_SCRIPT).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        their_hex.clone(),
        "--pn".into(),
        silent_hex,
        "--timeout".into(),
        "15".into(),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x16".into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    // The correspondent appears once the send runs, as in the late session.
    let storage = tempfile::tempdir().expect("their storage");
    let mut them = None;
    let deadline = Instant::now() + Duration::from_secs(40);
    while !child.is_finished() && Instant::now() < deadline {
        let text = std::fs::read_to_string(&frames).unwrap_or_default();
        if text.contains(": sending ---") {
            them = Some(
                attach(AttachConfig {
                    instance: daemon.instance.clone(),
                    config_dir: Some(daemon.config_dir.path().to_path_buf()),
                    storage_dir: storage.path().to_path_buf(),
                    identity: theirs,
                    display_name: b"them".to_vec(),
                    notices: Terminal::new(storage.path().to_path_buf()),
                })
                .await
                .expect("the correspondent attaches"),
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let output = child.await.expect("the loquax child thread");
    if let Some(them) = them {
        them.stop().await;
    }
    let got = std::fs::read_to_string(&frames).unwrap_or_default();
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "the send waited behind the fetch: stderr: {}\nframes:\n{got}\ntui.log:\n{log}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The fetch was still running when the send went: it began before the
    // send and had not ended when the send's direct leg started.
    let fetch_at = log.find("SYNC reason=key").expect("the fetch began");
    let send_at = log.find("SEND leg=direct").expect("the send began");
    assert!(fetch_at < send_at, "{log}");
    assert!(
        log.find("SYNC done").is_none_or(|done| done > send_at),
        "the fetch had ended before the send began: {log}"
    );
}

/// A fetch that fails says so in `tui.log`, with the reason it began for,
/// the failure's first line and how long it took (leviculum 1072): before,
/// only the start was logged, and a sync that ran its whole budget into a
/// failure left no trace. A node never heard has no path, and the drain asks
/// for one until its budget, four seconds at `--timeout 2`, runs out.
const FAILED_FETCH_SCRIPT: &str = "\
until ready 20
key :
type fetch
key enter
sleep 7000
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_fetch_leaves_its_end_in_the_log() {
    // The host's load over the run (1473, 1489).
    let host = loquax::host_load::HostLoad::start();
    let daemon = daemon("tui-fail", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    // The ugly store (1173): nothing on screen is under test here.
    ugly_store::fill_store(home.path());
    let script = home.path().join("fail.script");
    let frames = home.path().join("fail.frames");
    std::fs::write(&script, FAILED_FETCH_SCRIPT).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--pn".into(),
        "5e".repeat(16),
        "--timeout".into(),
        "2".into(),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x16".into(),
    ];
    let child_home = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    })
    .await
    .expect("the loquax child thread");
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\ntui.log:\n{log}",
        String::from_utf8_lossy(&output.stderr)
    );
    let done = log
        .lines()
        .find_map(|line| line.split("SYNC done ").nth(1))
        .unwrap_or_else(|| panic!("no SYNC done line: {log}"));
    assert!(
        done.starts_with("reason=key result=failed detail=\"the mailbox sync failed: no path"),
        "{done}"
    );
    let elapsed: u64 = done
        .rsplit("elapsed_ms=")
        .next()
        .and_then(|ms| ms.trim().parse().ok())
        .unwrap_or_else(|| panic!("no elapsed_ms: {done}"));
    // The fetch's own timeout is 2 s: never sooner. How much later a
    // loaded host ends it is the host's, so that bar counts only on a
    // quiet one (1489).
    assert!(elapsed >= 2_000, "{done}");
    if host.measured("the failed fetch's end") {
        assert!(elapsed < 7_000, "{done}");
    }
}

/// A sender loquax has never heard (leviculum 1079): it attaches and
/// announces before loquax starts, so its announce never reaches loquax, and
/// `--pn` keeps the start from replaying the daemon's path table. Its message
/// arrives with no key to check it against, and turns verified once loquax
/// has asked for the key.
const UNHEARD_SCRIPT: &str = "\
until ready 20
frame attached
key enter
until received 1 30
until verified 1 15
frame verified
key esc
key q
";

/// A message from a sender loquax has not heard turns verified, under the
/// sender's announced name, once loquax has asked for the sender's key.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_from_an_unheard_sender_is_verified_once_its_key_arrives() {
    let daemon = daemon("tui-unheard", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let our_address = delivery_address(&ours).expect("our address");
    let theirs = fixed_identity(0x22);
    let their_address = delivery_address(&theirs).expect("their address");

    // The sender first: its one announce reaches the daemon before loquax
    // is there to hear it.
    let storage = tempfile::tempdir().expect("their storage");
    let mut them = attach(AttachConfig {
        instance: daemon.instance.clone(),
        config_dir: Some(daemon.config_dir.path().to_path_buf()),
        storage_dir: storage.path().to_path_buf(),
        identity: theirs,
        display_name: b"them".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("the sender attaches");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !daemon
        .node
        .has_path(&leviculum_core::DestinationHash::new(their_address))
    {
        assert!(
            Instant::now() < deadline,
            "the daemon never heard the sender"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let script = home.path().join("unheard.script");
    let frames = home.path().join("unheard.frames");
    std::fs::write(&script, UNHEARD_SCRIPT).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--pn".into(),
        "5e".repeat(16),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "120x24".into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    });

    wait_for_frame(&frames, "attached", Duration::from_secs(20)).await;
    let options = SendOptions::new(daemon.instance.clone(), Duration::from_secs(20));
    let mut fanout = Fanout::default();
    let mut tap = fanout.tap();
    let clock = Tokio::new();
    let send = run_send(
        &mut tap,
        SendRequest {
            destination: our_address,
            title: Vec::new(),
            body: b"you have not heard of me".to_vec(),
            fields: Vec::new(),
            via: Via::Direct,
        },
        &options,
        &clock,
    );
    tokio::pin!(send);
    let sent = loop {
        tokio::select! {
            sent = &mut send => break sent,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {
                let _ = fanout.pump(&mut them.outbox);
            }
        }
    };
    assert!(sent.is_ok(), "the message did not go out: {sent:?}");

    let output = child.await.expect("the loquax child thread");
    them.stop().await;
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\nframes:\n{got}\ntui.log:\n{log}",
        String::from_utf8_lossy(&output.stderr)
    );
    // It arrived unverified: the engine asked for the sender's path, and
    // checked the signature once the answer brought the key.
    let source = their_address[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert!(
        log.contains(&format!(
            "VERIFY path_request src={source} held=1 requests=1"
        )),
        "{log}"
    );
    assert!(
        log.lines().any(
            |line| line.contains(&format!("VERIFY result src={source} id="))
                && line.ends_with("verdict=valid")
        ),
        "{log}"
    );
    let verified = frame(&got, "verified");
    assert!(
        !verified.contains("unverified") && verified.contains("them"),
        "the block did not turn verified under the sender's name:\n{verified}\ntui.log:\n{log}"
    );
}

/// 1162: the palette's `import` reads the NomadNet tree in its default
/// place, and the running screen lists the conversation under the imported
/// name and shows its messages when it is opened: no restart.
#[tokio::test(flavor = "multi_thread")]
async fn the_palettes_import_shows_at_once() {
    let daemon = daemon("tui-import", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    // The user's home the scan looks in, never the real one.
    let user = tempfile::tempdir().expect("user home");
    nomadnet_tree::write(
        &user.path().join(".nomadnetwork"),
        &fixed_identity(0x11),
        &fixed_identity(0x22),
    );
    // The toast with the import's report covers the newest rows; the
    // opened conversation is drawn once it is gone.
    let script = "until ready 20\nframe empty\nkey :\ntype import\nkey enter\n\
                  frame imported\nkey enter\nsleep 5000\nframe opened\nkey q\n";
    let script_path = home.path().join("import.script");
    let frames = home.path().join("import.frames");
    std::fs::write(&script_path, script).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--script".into(),
        script_path.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x24".into(),
    ];
    let (child_home, user_home) = (home.path().to_path_buf(), user.path().to_path_buf());
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("HOME", &user_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    })
    .await
    .expect("the loquax child thread");
    let got = std::fs::read_to_string(&frames).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (before, after) = got.split_once(": imported ---").expect("the frames");
    assert!(!before.contains(nomadnet_tree::NAME), "{got}");
    let (imported, opened) = after.split_once(": opened ---").expect("both frames");
    assert!(
        imported.contains(nomadnet_tree::NAME),
        "the list shows the imported conversation under its name:\n{got}"
    );
    assert!(
        opened.contains(nomadnet_tree::SENT) && opened.contains(nomadnet_tree::RECEIVED),
        "the opened conversation shows the imported rows:\n{got}"
    );
}

/// The ugly store's session (1173): a terminal loquax whose store holds a
/// year of use and two imports (`support/ugly_store.rs`) lists its
/// conversations, goes to the bottom of the list, where the Ratspeak
/// import lies, opens it and scrolls to its oldest row. The clock is the
/// script's, so every date on screen is the same on every run. Pinned in
/// `tests/fixtures/tui_ugly.frames`.
#[tokio::test(flavor = "multi_thread")]
async fn an_imported_conversation_opens_and_scrolls_to_its_oldest_row() {
    same_session(
        "tui_ugly.frames",
        &an_imported_conversation_opens_and_scrolls_to_its_oldest_row_in("").await,
    );
}

/// [`an_imported_conversation_opens_and_scrolls_to_its_oldest_row`] as a German desktop sees it (`LANG=de_DE.UTF-8`): every
/// script of the session after `lang de`, the step the locale takes.
#[tokio::test(flavor = "multi_thread")]
async fn an_imported_conversation_opens_and_scrolls_to_its_oldest_row_in_german() {
    let frames = an_imported_conversation_opens_and_scrolls_to_its_oldest_row_in("lang de\n").await;
    assert!(frames.contains("Unbekannt"), "{frames}");
    same_session("tui_ugly_de.frames", &frames);
}

/// The session of [`an_imported_conversation_opens_and_scrolls_to_its_oldest_row`] with `lang` before every script: its frames.
async fn an_imported_conversation_opens_and_scrolls_to_its_oldest_row_in(lang: &str) -> String {
    let daemon = daemon("tui-ugly", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    let filled = ugly_store::fill_store(home.path());
    assert_eq!(filled.imported, 2 * ugly_store::PER_CONVERSATION);
    let script = "clock 2026-10-31 12:00\nuntil ready 20\nframe list\nkey G\n\
                  frame bottom\nkey enter\nframe opened\nkey g\nframe oldest\nkey q\n";
    let script_path = home.path().join("ugly.script");
    let frames = home.path().join("ugly.frames");
    std::fs::write(&script_path, format!("{lang}{script}")).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--script".into(),
        script_path.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x24".into(),
    ];
    let child_home = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
        child.wait_with_output().expect("wait for loquax")
    })
    .await
    .expect("the loquax child thread");
    let got = std::fs::read_to_string(&frames).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    got
}

/// A messenger run whose own state lives in `lnmsg_home` and whose user's
/// home is `home`, with `lnsd` as the daemon it may start, no display to
/// open a window on, and pipes for its input and output. Returns the exit code,
/// stdout and stderr.
fn run_messenger(
    args: &[&str],
    lang: &str,
    lnmsg_home: &Path,
    home: &Path,
    lnsd: &Path,
) -> (Option<i32>, String, String) {
    let mut command = Command::new(LNMSG);
    for name in [
        "LC_ALL",
        "LC_MESSAGES",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "LEVICULUM_EVENT_LOG",
        "RUST_LOG",
    ] {
        command.env_remove(name);
    }
    command
        .args(args)
        .env("LANG", lang)
        .env("LNMSG_HOME", lnmsg_home)
        .env("HOME", home)
        .env("LEVICULUM_LNSD", lnsd)
        // A started daemon brings up no medium of its own: no AutoInterface
        // on the host's network, no serial or Bluetooth scan.
        .env("LEVICULUM_NO_AUTO", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
    let output = child.wait_with_output().expect("wait for loquax");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Every entry under `dir`, for an assertion that names what is there.
fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|it| {
            it.flatten()
                .map(|e| e.path().display().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The messenger started with no daemon and no lnsd to start (1265, walk
/// finding 5; since 1550 it starts one where it can, 1570): before any frame
/// is drawn it says one sentence and one command, in the locale's language,
/// and exits 1, as plain `loquax` and as `loquax tui`. No lnsd is reachable:
/// `LEVICULUM_LNSD` names none, and the user's home is empty. This binary
/// is a checkout's build, so the command is the checkout's; the packaged
/// `sudo systemctl start lnsd` is `start_command`'s unit test. The stack's
/// words (the instance, the shared instance, rnsd) only with `--verbose`.
#[test]
fn without_a_daemon_or_an_lnsd_to_start_the_messenger_says_one_sentence_and_one_command() {
    let instance = format!("loquax-nodaemon-tui-{}", std::process::id());
    let missing = Path::new("/nonexistent/lnsd-1570");
    let run = |args: &[&str], lang: &str| {
        let lnmsg_home = tempfile::tempdir().expect("state dir");
        let home = tempfile::tempdir().expect("user home");
        let got = run_messenger(args, lang, lnmsg_home.path(), home.path(), missing);
        assert!(
            entries(home.path()).is_empty(),
            "{args:?} wrote into the user's home: {:?}",
            entries(home.path())
        );
        got
    };
    let expected = [
        (
            "C.UTF-8",
            "loquax: Loquax needs its network service. Start it with: \
             cargo run --release --bin lnsd\n",
        ),
        (
            "de_DE.UTF-8",
            "loquax: Loquax braucht seinen Netzwerkdienst. Starte ihn mit: \
             cargo run --release --bin lnsd\n",
        ),
    ];
    for (lang, screen) in expected {
        for args in [
            &["--instance", &instance][..],
            &["tui", "--instance", &instance][..],
        ] {
            let (code, stdout, stderr) = run(args, lang);
            assert_eq!(code, Some(1), "{lang} {args:?} stderr: {stderr}");
            assert!(stdout.is_empty(), "{lang} {args:?} stdout: {stdout}");
            assert_eq!(stderr, screen, "{lang} {args:?}");
        }
    }

    let (code, _, stderr) = run(&["--instance", &instance, "--verbose"], "C.UTF-8");
    assert_eq!(code, Some(1), "stderr: {stderr}");
    for words in [
        instance.as_str(),
        "shared instance",
        "rnsd",
        "/nonexistent/lnsd-1570",
    ] {
        assert!(stderr.contains(words), "--verbose lacks {words}: {stderr}");
    }
}

/// The checkout's release lnsd, which `just build-integ-bins` builds (the
/// `loquax-tui` recipe depends on it), refused by name when it is older
/// than the sources it is built from: an old build has no
/// `--instance-name` and would fail this test for a reason the tree does
/// not have (the mvrs' check, leviculum#507).
fn release_lnsd() -> std::path::PathBuf {
    let lnsd = Path::new(LNMSG)
        .parent()
        .and_then(Path::parent)
        .expect("the target directory")
        .join("release")
        .join(format!("lnsd{}", std::env::consts::EXE_SUFFIX));
    let built = std::fs::metadata(&lnsd)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| {
            panic!(
                "{} not found ({e}): run `just build-integ-bins` first, or `just loquax-tui`, \
                 which builds it",
                lnsd.display()
            )
        });
    fn newest(dir: &Path, found: &mut Option<(std::time::SystemTime, std::path::PathBuf)>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                newest(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(m) = entry.metadata().and_then(|m| m.modified()) {
                    if found.as_ref().is_none_or(|(t, _)| m > *t) {
                        *found = Some((m, path));
                    }
                }
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = None;
    for dir in [
        "leviculum-core/src",
        "leviculum-std/src",
        "leviculum-cli/src",
        "leviculum-lxmf/src",
    ] {
        newest(&root.join(dir), &mut found);
    }
    if let Some((touched, source)) = found {
        assert!(
            built >= touched,
            "{} is older than {}: run `just build-integ-bins` first, or `just loquax-tui`, \
             which builds it",
            lnsd.display(),
            source.display()
        );
    }
    lnsd
}

/// The messenger with an lnsd to start and no daemon running (1570): it
/// starts the daemon in its own state directory, `$LNMSG_HOME/reticulum`,
/// on the instance it was asked for, and never in the user's
/// `~/.reticulum` (the user's home here is a second empty directory, and
/// stays empty). The daemon answers, so the messenger goes on to its
/// screen; its input and output are pipes, so it says it needs a terminal
/// and exits 1. The daemon goes with it:
/// the log shows the stop, and the pid the `INSTANCE` line named is gone.
#[test]
fn with_an_lnsd_to_start_the_messenger_starts_it_in_its_own_home_and_it_goes_with_it() {
    let lnsd = release_lnsd();
    for (n, front) in [&[][..], &["tui"][..]].into_iter().enumerate() {
        let instance = format!("loquax-starts-{}-{n}", std::process::id());
        let lnmsg_home = tempfile::tempdir().expect("state dir");
        let home = tempfile::tempdir().expect("user home");
        let mut args: Vec<&str> = front.to_vec();
        args.extend(["--instance", &instance]);
        let (code, stdout, stderr) =
            run_messenger(&args, "C.UTF-8", lnmsg_home.path(), home.path(), &lnsd);
        assert_eq!(code, Some(1), "{args:?} stderr: {stderr}");
        assert!(stdout.is_empty(), "{args:?} stdout: {stdout}");
        assert_eq!(
            stderr,
            "loquax: the terminal view needs a terminal on its input and output; \
             run it in one, or \"loquax gui\" for the window\n",
            "{args:?}: the daemon answered, so the screen is what failed"
        );

        assert!(
            entries(home.path()).is_empty(),
            "{args:?}: the started daemon wrote into the user's home: {:?}",
            entries(home.path())
        );
        let dir = lnmsg_home.path().join("reticulum");
        for file in ["config", "lnsd.log", "storage"] {
            assert!(
                dir.join(file).exists(),
                "{args:?}: no {file} in {}: {:?}",
                dir.display(),
                entries(&dir)
            );
        }
        let daemon_log = std::fs::read_to_string(dir.join("lnsd.log")).expect("lnsd.log");
        assert!(
            daemon_log.contains(&format!("Instance name: {instance} (--instance-name)")),
            "{args:?}: the daemon serves another name:\n{daemon_log}"
        );
        assert!(
            daemon_log.contains(&format!("Config dir: {}", dir.display())),
            "{args:?}: the daemon runs elsewhere:\n{daemon_log}"
        );
        assert!(
            daemon_log.contains("Received SIGTERM"),
            "{args:?}: the daemon was not stopped with the messenger:\n{daemon_log}"
        );

        let tui_log = std::fs::read_to_string(lnmsg_home.path().join("tui.log")).expect("tui.log");
        let line = tui_log
            .lines()
            .find(|l| l.contains("INSTANCE") && l.contains("started"))
            .unwrap_or_else(|| panic!("{args:?}: no INSTANCE started line:\n{tui_log}"));
        let pid: u32 = line
            .split("pid=")
            .nth(1)
            .and_then(|rest| {
                rest.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .unwrap_or_else(|| panic!("{args:?}: no pid in {line}"));
        let stat = Path::new("/proc").join(pid.to_string()).join("stat");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // Gone, a zombie its new parent has yet to reap, or the pid
            // already reused by another program: no lnsd running there.
            let alive = std::fs::read_to_string(&stat).ok().is_some_and(|s| {
                let state = s.rsplit(')').next().unwrap_or("").trim_start();
                s.contains("(lnsd)") && !state.starts_with('Z')
            });
            if !alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{args:?}: lnsd {pid} outlived the messenger"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// An lnsd the test runs and stops, killed on drop so a failed assertion
/// leaves none behind.
struct RunningLnsd(Option<std::process::Child>);

impl RunningLnsd {
    /// Stop it and wait, so its log is complete.
    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for RunningLnsd {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Wait until the daemon's `log` holds at least `wanted` `Local client
/// connected` lines (`leviculum-std/src/interfaces/local.rs`), and return
/// how many it holds. The line is written by the daemon's accept task,
/// which is asynchronous to the client's `connect()`: that lands in the
/// kernel's backlog and succeeds with the daemon not yet scheduled, and the
/// messenger runs its whole start and exits against such a daemon (the land
/// gate on a0cbebd4, loaded host). Bounded: a daemon that stays below
/// `wanted` fails with `context` and its log.
fn wait_for_client_connects(log: &Path, wanted: usize, context: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let count = text.matches("Local client connected").count();
        if count >= wanted {
            return count;
        }
        assert!(
            Instant::now() < deadline,
            "{context}: the daemon logged {count} client connect(s), waited for {wanted}:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Start the release lnsd on `config_dir`, its output in `log`, and wait
/// until its shared instance `instance` answers.
fn run_lnsd(lnsd: &Path, config_dir: &Path, instance: &str, log: &Path) -> RunningLnsd {
    let out = std::fs::File::create(log).expect("lnsd log");
    let err = out.try_clone().expect("lnsd log");
    let mut command = Command::new(lnsd);
    command
        .arg("-c")
        .arg(config_dir)
        .env("LEVICULUM_NO_AUTO", "1")
        .env_remove("LEVICULUM_EVENT_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    let running = RunningLnsd(Some(
        leviculum_std::process::spawn_supervised(command).expect("spawn lnsd"),
    ));
    let config = leviculum_std::config::Config::load(config_dir.join("config")).expect("config");
    let deadline = Instant::now() + Duration::from_secs(10);
    while leviculum_std::instance::find(Some(&config), instance).is_none() {
        assert!(
            Instant::now() < deadline,
            "lnsd on {} never answered on {instance}:\n{}",
            config_dir.display(),
            std::fs::read_to_string(log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    running
}

/// 1598: the messenger with only `LNMSG_HOME` set (no `--config`, no
/// `--instance`) finds the user's running lnsd, whose config in
/// `~/.reticulum` names its own instance, and starts none in
/// `$LNMSG_HOME/reticulum`. Since 1570 it read the instance name from the
/// moved home only, asked for `default`, found nothing and started a daemon
/// of its own beside the node's (the read-receipt conformance cell, red on
/// the land gate of 2c68a84b).
#[test]
fn with_only_lnmsg_home_set_the_messenger_finds_the_users_running_daemon() {
    let lnsd = release_lnsd();
    for (n, front) in [&[][..], &["tui"][..]].into_iter().enumerate() {
        let instance = format!("loquax-finds-{}-{n}", std::process::id());
        let lnmsg_home = tempfile::tempdir().expect("state dir");
        let home = tempfile::tempdir().expect("user home");
        let users = home.path().join(".reticulum");
        std::fs::create_dir(&users).expect("~/.reticulum");
        std::fs::write(
            users.join("config"),
            format!(
                "[reticulum]\n  share_instance = Yes\n  instance_name = {instance}\n\n[interfaces]\n"
            ),
        )
        .expect("config");
        let daemon_log = home.path().join("running-lnsd.log");
        let mut running = run_lnsd(&lnsd, &users, &instance, &daemon_log);
        // `run_lnsd`'s own find probe is one connect: logged before the
        // messenger runs, so what the messenger adds is counted from here.
        let baseline = wait_for_client_connects(&daemon_log, 1, "the harness's probe");

        let (code, stdout, stderr) =
            run_messenger(front, "C.UTF-8", lnmsg_home.path(), home.path(), &lnsd);
        assert_eq!(code, Some(1), "{front:?} stderr: {stderr}");
        assert!(stdout.is_empty(), "{front:?} stdout: {stdout}");
        assert_eq!(
            stderr,
            "loquax: the terminal view needs a terminal on its input and output; \
             run it in one, or \"loquax gui\" for the window\n",
            "{front:?}: the daemon answered, so the screen is what failed"
        );
        // The messenger's connects (the lookup's probe, the engine) land in
        // the kernel's backlog, and its exit does not wait for the daemon's
        // accept: the daemon is stopped only once it logged them.
        wait_for_client_connects(
            &daemon_log,
            baseline + 1,
            &format!("{front:?}: the messenger never reached the running daemon"),
        );
        running.stop();

        let started = lnmsg_home.path().join("reticulum").join("lnsd.log");
        assert!(
            !started.exists(),
            "{front:?}: the messenger started a daemon of its own:\n{}",
            std::fs::read_to_string(&started).unwrap_or_default()
        );
        let tui_log = std::fs::read_to_string(lnmsg_home.path().join("tui.log")).expect("tui.log");
        let line = tui_log
            .lines()
            .find(|l| l.contains("INSTANCE"))
            .unwrap_or_else(|| panic!("{front:?}: no INSTANCE line:\n{tui_log}"));
        assert!(
            line.contains("found") && !line.contains("started"),
            "{front:?}: {line}"
        );
        assert!(line.contains("source=default-dir"), "{front:?}: {line}");
    }
}

/// 1598's control: with only `LNMSG_HOME` set and no daemon on any name the
/// lookup asks for, the messenger still starts one, in its own home
/// (`source=none`, no config anywhere), as 1570 decided. The lookup asks
/// for `default` last, so a host that runs a default daemon of its own is
/// found there instead: the test asserts that branch where it holds.
#[test]
fn with_only_lnmsg_home_set_and_no_daemon_the_messenger_starts_one_in_its_home() {
    let lnsd = release_lnsd();
    let host_default = leviculum_std::instance::find(None, "default").is_some();
    let lnmsg_home = tempfile::tempdir().expect("state dir");
    let home = tempfile::tempdir().expect("user home");
    let (code, _, stderr) = run_messenger(&[], "C.UTF-8", lnmsg_home.path(), home.path(), &lnsd);
    assert_eq!(code, Some(1), "stderr: {stderr}");
    assert!(
        entries(home.path()).is_empty(),
        "the messenger wrote into the user's home: {:?}",
        entries(home.path())
    );
    let tui_log = std::fs::read_to_string(lnmsg_home.path().join("tui.log")).expect("tui.log");
    let line = tui_log
        .lines()
        .find(|l| l.contains("INSTANCE"))
        .unwrap_or_else(|| panic!("no INSTANCE line:\n{tui_log}"));
    assert!(line.contains("source=none"), "{line}");
    let started = lnmsg_home.path().join("reticulum").join("lnsd.log");
    match host_default {
        true => {
            assert!(line.contains("found"), "{line}");
            assert!(!started.exists(), "a daemon was started beside the host's");
        }
        false => {
            assert!(line.contains("started"), "{line}");
            let daemon_log = std::fs::read_to_string(&started).expect("lnsd.log");
            assert!(
                daemon_log.contains("Received SIGTERM"),
                "the daemon was not stopped with the messenger:\n{daemon_log}"
            );
        }
    }
}
