//! The map's fling (1188): `Fling.java`, the part of the phone's coast
//! that is plain Java, compiled with the JDK the APK's build needs (a
//! `default-jdk-headless` that `install-toolchain.sh` asks for, it does not
//! install it) and run with `tests/java/FlingTest.java`: which
//! gestures fling by a held clock (the minimum velocity, a pinch, a second
//! finger, a finger that stopped), the room a coast has to the world's
//! edge, and a coast frame by frame that slows down, converges, stops at
//! the edge and ends so the view is drawn at rest.
//!
//! The recorder's level feed (1276), `Levels.java`, runs the same way with
//! `tests/java/LevelsTest.java`: its windows are the library's.
//!
//! The service notice's rules (1345), `Notice.java`, run with
//! `tests/java/NoticeTest.java`: which change posts the notice again and
//! when a reboot starts the service.
//!
//! The engine's questions (1496), `Engine.java`, run with
//! `tests/java/EngineTest.java` and stand-ins for Android's `Handler`,
//! `Looper` and `Log` (`tests/java/stubs`): a question that throws reaches
//! its failure path.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn the_fling_decides_and_coasts_as_the_phone_does() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fling-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/Fling.java"))
        .arg(manifest.join("tests/java/FlingTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.FlingTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// The recorder's level feed (1276): `Levels.java`, the window cutting and
/// the RMS the phone hands the library every 50 ms, run on the host with
/// `tests/java/LevelsTest.java` over speech, loud full-scale sound and
/// digital silence, in odd pieces so a sample splits across two reads.
/// Window for window it is `loquax::ui::recording::rms` of the same
/// samples, as `LevelTrack::from_pcm` cuts them; the last, short window
/// stays with the recorder until more sound comes.
#[test]
fn the_recorders_windows_are_the_librarys() {
    use loquax::ui::recording::rms;
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("levels-classes");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&work)
        .arg(manifest.join("android/src/network/leviculum/loquax/Levels.java"))
        .arg(manifest.join("tests/java/LevelsTest.java"))
        .output()
        .expect("javac runs");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let rate = 16_000u32;
    let mut samples = loquax::voice::test_speech(rate, 1_000);
    samples.extend((0..4_000).map(|i| if i % 2 == 0 { i16::MAX } else { i16::MIN }));
    samples.extend(std::iter::repeat_n(0, 8_000));
    samples.extend([5, -5, 3]);
    let file = work.join("pcm.raw");
    std::fs::write(
        &file,
        samples
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
    .expect("the PCM");
    let window = (rate / 20) as usize;
    let want: Vec<String> = samples
        .chunks_exact(window)
        .map(|chunk| rms(chunk).to_string())
        .chain([format!("samples {window}")])
        .collect();
    assert!(want.iter().any(|w| w == "32767"), "full scale is in it");
    assert!(want.iter().any(|w| w == "0"), "silence is in it");
    for piece in ["333", "1600", "7"] {
        let ran = Command::new("java")
            .arg("-ea")
            .arg("-cp")
            .arg(&work)
            .arg("network.leviculum.loquax.LevelsTest")
            .arg(&file)
            .arg(rate.to_string())
            .arg(piece)
            .output()
            .expect("java runs");
        assert!(
            ran.status.success(),
            "{}",
            String::from_utf8_lossy(&ran.stderr)
        );
        let got: Vec<String> = String::from_utf8_lossy(&ran.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(got, want, "pieces of {piece} bytes");
    }
}

/// The service notice (1345): `Notice.java` run on the host with
/// `tests/java/NoticeTest.java`. A change of the sentence (connected, not
/// connected, the mailbox here) posts the notice again, a same sentence
/// and an unread change do not, so a swipe on Android 14 holds; a reboot
/// starts the service only on BOOT_COMPLETED and only when the user had
/// it running.
#[test]
fn the_service_notice_posts_on_a_change_and_the_boot_starts_what_ran() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("notice-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/Notice.java"))
        .arg(manifest.join("tests/java/NoticeTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.NoticeTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// The join screen's server field follows the code's tail (1464):
/// `ServerField.java` run on the host with `tests/java/ServerFieldTest.java`.
/// A code typed one character at a time ends with the whole server in the
/// field; the control in the Java, the rule before 1464, ends with the
/// stray "1" the bench saw.
#[test]
fn the_server_field_follows_a_typed_code() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("server-field-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/ServerField.java"))
        .arg(manifest.join("tests/java/ServerFieldTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.ServerFieldTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// The join form's rules (1448, walk finding 4): `JoinForm.java` with
/// `ServerField.java` run on the host with `tests/java/JoinFormTest.java`.
/// The form's own start without a code says no failure, a join the user
/// asked for does; the address field is offered only under a code without
/// servers. The control in the Java, the rule before, says a failure on the
/// bare form.
#[test]
fn the_join_form_says_a_failure_only_after_a_join() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("join-form-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/ServerField.java"))
        .arg(manifest.join("android/src/network/leviculum/loquax/JoinForm.java"))
        .arg(manifest.join("tests/java/JoinFormTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.JoinFormTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// A new conversation's picker rows (1448, walk finding 17):
/// `PeopleRows.java` run on the host with `tests/java/PeopleRowsTest.java`.
/// The group picker is the people's list with check marks, in their order
/// and with their look; the control, the dialog's rows before, has no look.
#[test]
fn the_group_picker_is_the_people_list_with_check_marks() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("people-rows-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/PeopleRows.java"))
        .arg(manifest.join("tests/java/PeopleRowsTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.PeopleRowsTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// The Explore pane's rules (1618, the walk of 17024c3a):
/// `ExploreRows.java` run on the host with
/// `tests/java/ExploreRowsTest.java`. 32 characters typed fast all stay
/// in the field (the control, the rule before, loses some, as the Pixel
/// lost 10 of 32), a row under a finger stays while a new order
/// arrives (the control lands on the walk's wrong row), and (1621, walk 2)
/// the row under a planned tap stays at its pixel across ten new pages,
/// scrolled and at the top, the chip counting those that wait (the
/// control, ListView by position, moves it).
#[test]
fn the_explore_field_keeps_every_character_and_rows_hold_under_a_finger() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("explore-rows-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/ExploreRows.java"))
        .arg(manifest.join("tests/java/ExploreRowsTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.ExploreRowsTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// When nothing changes, nothing is drawn (1455): `Quiet.java`, the rules
/// the phone's screens ask before drawing again, run on the host with
/// `tests/java/QuietTest.java`. The map's live ring pulses for its first
/// cycles and then asks no frame; the 30-second tick of the chat list
/// draws nothing while the library's answer stays the same. The controls
/// in the Java: the pulse's seconds, the endless pulse before 1455, a row
/// whose age moved.
#[test]
fn the_phone_draws_nothing_while_nothing_changes() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("quiet-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/Quiet.java"))
        .arg(manifest.join("tests/java/QuietTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.QuietTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// A question that throws reaches its failure path (1496): `Engine.java`
/// run on the host with `tests/java/EngineTest.java` over stand-ins for
/// Android's `Handler`, `Looper` and `Log`. Before, a throw in `Engine.ask`
/// left `got` unsaid, and `ChatActivity.asking` stayed set for good.
#[test]
fn a_question_that_throws_reaches_its_failure_path() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("engine-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let stubs = manifest.join("tests/java/stubs/android");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(stubs.join("os/Looper.java"))
        .arg(stubs.join("os/Handler.java"))
        .arg(stubs.join("util/Log.java"))
        .arg(manifest.join("android/src/network/leviculum/loquax/Engine.java"))
        .arg(manifest.join("tests/java/EngineTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.EngineTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// The chat's Back (1449 item 1, walk finding 5): `BackRule.java` run on
/// the host with `tests/java/BackRuleTest.java`. A reply being written
/// survives Back: the keyboard closes first, the screen leaves with the
/// reply in the draft. The control in the Java, the rule before, ended it.
#[test]
fn back_closes_the_keyboard_and_keeps_the_reply() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("back-rule-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/BackRule.java"))
        .arg(manifest.join("tests/java/BackRuleTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.BackRuleTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// The mailbox page holds still (1647, walk 6, finding 28):
/// `MailboxLines.java` run on the host with
/// `tests/java/MailboxLinesTest.java`. Twelve asks five seconds apart fit
/// the page on screen and are written into its lines, so no view goes and
/// the scroll stays (the control, comparing words, sees a change on
/// nearly every ask, which the rule before drew into an emptied page); an
/// answer of another shape is drawn whole.
#[test]
fn the_mailbox_page_writes_a_fitting_answer_into_its_lines() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("mailbox-lines-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/MailboxLines.java"))
        .arg(manifest.join("tests/java/MailboxLinesTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.MailboxLinesTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("checks passed"), "{out}");
}

/// The form field's editor (1667, walk 11, finding 59): `FormField.java`
/// run on the host with `tests/java/FormFieldTest.java`. The dialog's hint
/// is what the page writes before the field on its line; a field opening
/// its line, or one no line holds (the control), has none.
#[test]
fn a_form_fields_hint_is_the_pages_words_before_it() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let classes = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("form-field-classes");
    let _ = std::fs::remove_dir_all(&classes);
    std::fs::create_dir_all(&classes).expect("a class directory");
    let compiled = Command::new("javac")
        .args(["-source", "8", "-target", "8", "-Xlint:-options", "-d"])
        .arg(&classes)
        .arg(manifest.join("android/src/network/leviculum/loquax/FormField.java"))
        .arg(manifest.join("tests/java/FormFieldTest.java"))
        .output()
        .expect("javac runs (a JDK on PATH; scripts/integ-prerequisites.txt skips this test where none is)");
    assert!(
        compiled.status.success(),
        "javac:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let ran = Command::new("java")
        .arg("-ea")
        .arg("-cp")
        .arg(&classes)
        .arg("network.leviculum.loquax.FormFieldTest")
        .output()
        .expect("java runs (the same JDK)");
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        ran.status.success(),
        "{out}\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert!(out.contains("5 checks passed"), "{out}");
}
