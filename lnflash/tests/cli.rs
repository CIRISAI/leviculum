//! The binary, driven the way a stranger drives it.
//!
//! Everything here runs against the fixture sysfs tree, so no device is
//! enumerated, touched or written — these tests are safe on a host with the
//! rig attached, and they pass on a host with nothing attached at all.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const EXE: &str = env!("CARGO_BIN_EXE_lnflash");

fn fixture_sysfs() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"))
}

/// An unpacked bundle: binary at the top, payload under `firmware/`, exactly
/// the layout `just lnflash-bundle` produces.
fn unpacked_bundle() -> TempDir {
    let dir = TempDir::new().unwrap();
    let firmware = dir.path().join("firmware/t114");
    fs::create_dir_all(&firmware).unwrap();

    let hex = include_str!("../payload/t114/s140_nrf52_7.3.0_softdevice.hex");
    let licence = include_str!("../payload/t114/s140_nrf52_7.3.0_license-agreement.txt");
    // A minimal but real UF2 at the application base.
    let app = lnflash::uf2::Image::from_spans(
        &[lnflash::ihex::Span {
            start: 0x2_7000,
            data: vec![0x5A; 0x800],
        }],
        lnflash::uf2::FAMILY_NRF52840_APP,
    )
    .encode()
    .unwrap();

    fs::write(firmware.join("s140_nrf52_7.3.0_softdevice.hex"), hex).unwrap();
    fs::write(
        firmware.join("s140_nrf52_7.3.0_license-agreement.txt"),
        licence,
    )
    .unwrap();
    fs::write(firmware.join("leviculum-t114-0.8.0.uf2"), &app).unwrap();
    fs::write(
        dir.path().join("firmware/manifest.toml"),
        manifest_text(
            &lnflash::manifest::hex_digest(&app),
            &lnflash::manifest::hex_digest(hex.as_bytes()),
        ),
    )
    .unwrap();
    dir
}

fn manifest_text(app_sha: &str, sd_sha: &str) -> String {
    format!(
        r#"
[bundle]
version = "0.8.0"
built   = "2026-08-10"

[board.t114]
family    = "nrf52840"
transport = "uf2-msc"
entry     = ["touch-1200", "double-tap"]

[board.t114.identify]
info_uf2_board_id = "HT-n5262"
bootloader_usb    = ["239a:0071"]
candidate_usb     = ["1209:0001", "239a:8071"]
msc_label         = "HT-n5262"

[board.t114.flash]
family_id      = 0xADA52840
writable_start = 0x1000
writable_end   = 0xEA000
app_base       = 0x27000

[board.t114.app]
file    = "t114/leviculum-t114-0.8.0.uf2"
sha256  = "{app_sha}"
git_sha = "bb7c4f64"

[board.t114.requires]
softdevice = ">=7.0.1, <8.0.0"

[board.t114.remedy.softdevice]
file    = "t114/s140_nrf52_7.3.0_softdevice.hex"
sha256  = "{sd_sha}"
license = "t114/s140_nrf52_7.3.0_license-agreement.txt"
convert = "hex-to-uf2"
"#
    )
}

/// Run the binary with no `$LNFLASH_BUNDLE` unless one is given, so the
/// developer's own environment cannot decide a test's outcome.
fn run(args: &[&str], env: Option<(&str, &Path)>) -> Output {
    let mut cmd = Command::new(EXE);
    cmd.args(args).env_remove(lnflash::manifest::BUNDLE_ENV);
    if let Some((key, value)) = env {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_unpacked_bundle_is_found_by_pointing_at_its_root() {
    let bundle = unpacked_bundle();
    let out = run(
        &[
            "--bundle",
            &bundle.path().display().to_string(),
            "--check-bundle",
        ],
        None,
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("matches its recorded checksum"));
    assert!(stdout(&out).contains("carrying t114"));
}

#[test]
fn the_environment_variable_finds_the_bundle_too() {
    let bundle = unpacked_bundle();
    let out = run(
        &["--check-bundle"],
        Some((lnflash::manifest::BUNDLE_ENV, bundle.path())),
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("matches its recorded checksum"));
}

#[test]
fn an_explicit_bundle_beats_the_environment_variable() {
    let good = unpacked_bundle();
    let broken = TempDir::new().unwrap();
    fs::create_dir_all(broken.path().join("firmware")).unwrap();
    fs::write(
        broken.path().join("firmware/manifest.toml"),
        "not a manifest",
    )
    .unwrap();

    let out = run(
        &[
            "--bundle",
            &good.path().display().to_string(),
            "--check-bundle",
        ],
        Some((lnflash::manifest::BUNDLE_ENV, broken.path())),
    );
    assert!(out.status.success(), "{}", stdout(&out));
}

#[test]
fn a_tampered_payload_fails_the_bundle_check_and_the_exit_code_says_so() {
    let bundle = unpacked_bundle();
    fs::write(
        bundle.path().join("firmware/t114/leviculum-t114-0.8.0.uf2"),
        b"not the image the manifest recorded",
    )
    .unwrap();
    let out = run(
        &[
            "--bundle",
            &bundle.path().display().to_string(),
            "--check-bundle",
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("sha256"));
}

#[test]
fn a_bundle_missing_the_nordic_licence_will_not_even_load() {
    let bundle = unpacked_bundle();
    fs::remove_file(
        bundle
            .path()
            .join("firmware/t114/s140_nrf52_7.3.0_license-agreement.txt"),
    )
    .unwrap();
    let out = run(
        &[
            "--bundle",
            &bundle.path().display().to_string(),
            "--check-bundle",
        ],
        None,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("s140_nrf52_7.3.0_license-agreement.txt"));
}

#[test]
fn no_bundle_anywhere_names_every_place_it_looked() {
    let nowhere = TempDir::new().unwrap();
    let out = run(
        &[
            "--bundle",
            &nowhere.path().join("absent").display().to_string(),
        ],
        None,
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("absent"), "{err}");
    assert!(err.contains("/usr/share/lnflash"), "{err}");
}

#[test]
fn a_dry_run_reports_both_boards_and_writes_nothing() {
    let bundle = unpacked_bundle();
    let out = run(
        &[
            "--bundle",
            &bundle.path().display().to_string(),
            "--dry-run",
            "--sysfs",
            &fixture_sysfs().display().to_string(),
        ],
        None,
    );
    let said = stdout(&out);
    assert!(out.status.success(), "{said}");
    // The application on 3-2.3.1 and its bootloader on 3-2.4 are both found;
    // the RAK on 3-2.3.4.4 is not a USB ID this bundle lists.
    assert!(said.contains("Found 2 device(s)"), "{said}");
    assert!(
        said.contains("3-2.3.1 [1209:0001] 183004F712B4A7FE"),
        "{said}"
    );
    assert!(
        said.contains("3-2.4 [239a:0071] 12B4A7FE183004F7"),
        "{said}"
    );
    assert!(!said.contains("3-2.3.4.4"), "{said}");
    // And it stops before doing anything to either.
    assert!(
        said.contains("rebooting a board is already a change"),
        "{said}"
    );
    assert!(!said.contains("copied"), "{said}");
}

#[test]
fn an_empty_bus_is_a_clean_report_rather_than_an_error_message() {
    let bundle = unpacked_bundle();
    let empty = TempDir::new().unwrap();
    let out = run(
        &[
            "--bundle",
            &bundle.path().display().to_string(),
            "--dry-run",
            "--sysfs",
            &empty.path().display().to_string(),
        ],
        None,
    );
    assert!(out.status.success());
    assert!(stdout(&out).contains("No board this bundle knows is attached"));
}

#[test]
fn a_run_that_was_supposed_to_write_and_did_not_exits_non_zero() {
    // Without --dry-run and with no board it can write to, the tool must not
    // report success: a CI job that flashes nothing has not flashed.
    let bundle = unpacked_bundle();
    let empty = TempDir::new().unwrap();
    let out = run(
        &[
            "--bundle",
            &bundle.path().display().to_string(),
            "--yes",
            "--sysfs",
            &empty.path().display().to_string(),
        ],
        None,
    );
    assert!(!out.status.success());
}

#[test]
fn the_help_text_says_it_needs_root_and_never_uses_the_network() {
    let out = run(&["--help"], None);
    assert!(out.status.success());
    let help = stdout(&out);
    assert!(help.contains("Needs root"), "{help}");
    assert!(help.contains("No network access"), "{help}");
    for flag in [
        "--dry-run",
        "--yes",
        "--bundle",
        "--board",
        "--check-bundle",
    ] {
        assert!(help.contains(flag), "{flag} missing from --help:\n{help}");
    }
}
