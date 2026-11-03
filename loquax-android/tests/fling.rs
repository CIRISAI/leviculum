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
