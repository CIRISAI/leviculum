//! The map's fling (1188): `Fling.java`, the part of the phone's coast
//! that is plain Java, compiled with the JDK the APK's build needs (a
//! `default-jdk-headless` that `install-toolchain.sh` asks for, it does not
//! install it) and run with `tests/java/FlingTest.java`: which
//! gestures fling by a held clock (the minimum velocity, a pinch, a second
//! finger, a finger that stopped), the room a coast has to the world's
//! edge, and a coast frame by frame that slows down, converges, stops at
//! the edge and ends so the view is drawn at rest.

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
