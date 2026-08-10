//! The bundle manifest: every board fact this tool knows.
//!
//! The binary is board-agnostic. An `if board == "t114"` anywhere outside
//! this module would mean the split has failed — a new nRF or RP2040 board
//! is meant to be data entry, and a new chip family exactly one new
//! transport (docs/src/concepts/lnode-flashing.md, "Four axes").
//!
//! Two properties are enforced here rather than documented:
//!
//! * **A payload cannot be read without its checksum being verified.**
//!   [`Payload::read`] is the only way to get the bytes and it always hashes
//!   them, so "verify the image checksum" is not a step anyone can forget.
//! * **A third-party blob cannot ship without its licence.** [`Remedy`] has a
//!   mandatory `license` field and loading fails if that file is missing.
//!   Nordic's clause 2 requires the notice to travel with the distribution;
//!   Meshtastic vendors the same blob without one.
//!
//! Nothing here is a programming language, and it must not become one. When
//! declarative data is not enough, the answer is a new transport in Rust
//! ("Deliberately not").

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::softdevice::VersionReq;
use crate::usb::UsbId;

/// Where the manifest lives inside a bundle.
pub const MANIFEST_NAME: &str = "manifest.toml";
/// The bundle subdirectory holding the manifest and the images.
pub const FIRMWARE_DIR: &str = "firmware";
/// Last resort in the resolution order, for a distro-packaged install.
pub const SYSTEM_BUNDLE: &str = "/usr/share/lnflash";
/// Environment override, second in the resolution order.
pub const BUNDLE_ENV: &str = "LNFLASH_BUNDLE";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no bundle found; looked in {}", .0.join(", "))]
    NoBundle(Vec<String>),
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{path}: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("{file}: sha256 is {actual}, manifest says {expected}")]
    Checksum {
        file: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("this bundle knows no board {wanted:?}; it carries {}", available.join(", "))]
    UnknownBoard {
        wanted: String,
        available: Vec<String>,
    },
    #[error("board {board}: {field} is not a version constraint: {source}")]
    BadConstraint {
        board: String,
        field: String,
        #[source]
        source: crate::softdevice::Error,
    },
    #[error("board {board}: {field} is not a vid:pid: {value:?}")]
    BadUsbId {
        board: String,
        field: String,
        value: String,
    },
}

/// How the bytes get in. One variant today; a new chip family adds one here
/// and one module, and touches nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    /// Copy a UF2 onto the bootloader's mass-storage drive.
    Uf2Msc,
}

/// How a board reaches a programmable state, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Entry {
    /// Open a CDC port at 1200 baud. Only works if the running firmware
    /// implements it; ours does, stock Meshtastic does not.
    // Spelled out because kebab-case renaming does not split before a digit.
    #[serde(rename = "touch-1200")]
    Touch1200,
    /// The bootloader's own mechanism. Works regardless of what is running,
    /// and needs a human — the load-bearing limit on any automatic tool.
    DoubleTap,
}

/// What has to happen to a payload file before it can be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Convert {
    /// Already a UF2; copy it as it is.
    None,
    /// Intel HEX, converted to UF2 at run time. Converting alters no byte,
    /// only the container, but distributing the untouched hex avoids even
    /// the appearance of the modification Nordic's clause 5 prohibits.
    HexToUf2,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub bundle: BundleInfo,
    #[serde(default)]
    pub board: BTreeMap<String, Board>,
    /// Directory the payload paths are relative to. Filled in by [`load`].
    #[serde(skip)]
    pub root: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleInfo {
    /// The firmware release this bundle carries.
    pub version: String,
    /// Free-form provenance for a user staring at a tarball of unknown age.
    #[serde(default)]
    pub built: Option<String>,
}

/// Everything this tool knows about one board — the four axes and the
/// preconditions crossing them.
#[derive(Debug, Clone, Deserialize)]
pub struct Board {
    /// Chip family, informational. `transport` is what decides behaviour.
    pub family: String,
    pub transport: Transport,
    pub entry: Vec<Entry>,
    pub identify: Identify,
    pub flash: Flash,
    pub app: Payload,
    #[serde(default)]
    pub requires: Requires,
    #[serde(default)]
    pub remedy: Remedy,
}

/// The **identify** axis, in the two stages the order of work demands.
#[derive(Debug, Clone, Deserialize)]
pub struct Identify {
    /// The truth, read from `INFO_UF2.TXT` after entering the bootloader.
    /// The only thing a write may rest on.
    pub info_uf2_board_id: String,
    /// USB IDs the bootloader answers on. Stage one only: it says a device
    /// is worth mounting, never what board it is.
    pub bootloader_usb: Vec<String>,
    /// USB IDs worth trying a touch on. Stage one only, and weaker still —
    /// these belong to whatever firmware is installed, not to the board.
    #[serde(default)]
    pub candidate_usb: Vec<String>,
    /// The mass-storage label the bootloader publishes. Reported to the
    /// user; never used to decide anything.
    #[serde(default)]
    pub msc_label: Option<String>,
}

/// Flash geometry, as measured from the bootloader's own `CURRENT.UF2`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Flash {
    /// UF2 family ID to write and to convert into. Never a bootloader family.
    pub family_id: u32,
    /// First address the bootloader accepts. Below it, blocks are skipped
    /// silently while still reporting success.
    pub writable_start: u32,
    /// One past the last address it accepts. At or above, blocks are rejected.
    pub writable_end: u32,
    /// Where the application lives, above the SoftDevice.
    pub app_base: u32,
}

/// One file in the bundle, and the checksum that says it arrived intact.
#[derive(Debug, Clone, Deserialize)]
pub struct Payload {
    /// Path relative to the manifest's directory.
    pub file: PathBuf,
    pub sha256: String,
    #[serde(default)]
    pub convert: Option<Convert>,
    /// The git SHA the image was built from, for the `[FW_BUILD]` banner
    /// check. Absent for third-party images, which emit no such banner.
    #[serde(default)]
    pub git_sha: Option<String>,
}

/// Preconditions. They cross all four axes, and each one names its own
/// remedy rather than becoming a special case in code.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Requires {
    #[serde(default)]
    pub softdevice: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Remedy {
    #[serde(default)]
    pub softdevice: Option<RemedyPayload>,
}

/// A payload that repairs an unmet precondition, plus the licence that has
/// to travel with it.
#[derive(Debug, Clone, Deserialize)]
pub struct RemedyPayload {
    #[serde(flatten)]
    pub payload: Payload,
    /// Mandatory. Loading fails if the file is absent, which is what makes
    /// shipping a third-party blob without its licence impossible.
    pub license: PathBuf,
}

impl Payload {
    /// Read the file and verify its checksum. The only way to obtain the
    /// bytes, so an unverified image cannot reach flash.
    pub fn read(&self, root: &Path) -> Result<Vec<u8>, Error> {
        let path = root.join(&self.file);
        let bytes = std::fs::read(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        let actual = hex_digest(&bytes);
        if !actual.eq_ignore_ascii_case(self.sha256.trim()) {
            return Err(Error::Checksum {
                file: path,
                expected: self.sha256.clone(),
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn path(&self, root: &Path) -> PathBuf {
        root.join(&self.file)
    }
}

impl Board {
    /// The SoftDevice constraint, parsed. `None` means the board states no
    /// SoftDevice precondition at all.
    pub fn softdevice_req(&self, name: &str) -> Result<Option<VersionReq>, Error> {
        self.requires
            .softdevice
            .as_deref()
            .map(|s| {
                VersionReq::parse(s).map_err(|source| Error::BadConstraint {
                    board: name.to_string(),
                    field: "requires.softdevice".into(),
                    source,
                })
            })
            .transpose()
    }

    pub fn bootloader_ids(&self, name: &str) -> Result<Vec<UsbId>, Error> {
        parse_ids(
            &self.identify.bootloader_usb,
            name,
            "identify.bootloader_usb",
        )
    }

    pub fn candidate_ids(&self, name: &str) -> Result<Vec<UsbId>, Error> {
        parse_ids(&self.identify.candidate_usb, name, "identify.candidate_usb")
    }
}

fn parse_ids(values: &[String], board: &str, field: &str) -> Result<Vec<UsbId>, Error> {
    values
        .iter()
        .map(|v| {
            v.parse::<UsbId>().map_err(|_| Error::BadUsbId {
                board: board.to_string(),
                field: field.to_string(),
                value: v.clone(),
            })
        })
        .collect()
}

impl Manifest {
    pub fn board(&self, name: &str) -> Result<&Board, Error> {
        self.board.get(name).ok_or_else(|| Error::UnknownBoard {
            wanted: name.to_string(),
            available: self.board.keys().cloned().collect(),
        })
    }

    /// The board whose `Board-ID` matches what the bootloader published.
    /// This is stage two of identify — the answer a write is allowed to rest
    /// on — so it matches exactly, never as a substring.
    pub fn board_for_id(&self, board_id: &str) -> Option<(&str, &Board)> {
        self.board
            .iter()
            .find(|(_, b)| b.identify.info_uf2_board_id == board_id)
            .map(|(name, b)| (name.as_str(), b))
    }

    pub fn names(&self) -> Vec<&str> {
        self.board.keys().map(String::as_str).collect()
    }

    /// Read and verify every payload in the bundle. What `--dry-run` runs so
    /// a user can check a tarball without a board attached.
    pub fn verify_all(&self) -> Result<(), Error> {
        for board in self.board.values() {
            board.app.read(&self.root)?;
            if let Some(remedy) = &board.remedy.softdevice {
                remedy.payload.read(&self.root)?;
            }
        }
        Ok(())
    }
}

/// Load and validate a manifest from the directory that holds it.
pub fn load(dir: &Path) -> Result<Manifest, Error> {
    let path = dir.join(MANIFEST_NAME);
    let text = std::fs::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    let mut manifest: Manifest = toml::from_str(&text).map_err(|source| Error::Toml {
        path: path.clone(),
        source,
    })?;
    manifest.root = dir.to_path_buf();
    validate(&manifest, &path)?;
    Ok(manifest)
}

fn validate(manifest: &Manifest, path: &Path) -> Result<(), Error> {
    let bad = |message: String| Error::Invalid {
        path: path.to_path_buf(),
        message,
    };
    if manifest.board.is_empty() {
        return Err(bad("a bundle with no boards in it".into()));
    }
    for (name, board) in &manifest.board {
        if board.identify.info_uf2_board_id.trim().is_empty() {
            return Err(bad(format!(
                "board {name}: identify.info_uf2_board_id is empty, so no board could ever \
                 be confirmed and no write could ever be safe"
            )));
        }
        if board.entry.is_empty() {
            return Err(bad(format!(
                "board {name}: no entry mechanism, so the bootloader is unreachable"
            )));
        }
        if board.identify.bootloader_usb.is_empty() {
            return Err(bad(format!(
                "board {name}: no identify.bootloader_usb, so the bootloader is unrecognisable"
            )));
        }
        board.bootloader_ids(name)?;
        board.candidate_ids(name)?;
        board.softdevice_req(name)?;

        if board.flash.family_id == crate::uf2::FAMILY_NRF52_BOOTLOADER {
            return Err(bad(format!(
                "board {name}: flash.family_id is the bootloader family; that image rewrites \
                 MBR, bootloader and UICR, and a failure there needs SWD to undo"
            )));
        }
        if board.flash.writable_start >= board.flash.writable_end {
            return Err(bad(format!(
                "board {name}: flash window {:#x}..{:#x} is empty",
                board.flash.writable_start, board.flash.writable_end
            )));
        }
        if !(board.flash.writable_start..board.flash.writable_end).contains(&board.flash.app_base) {
            return Err(bad(format!(
                "board {name}: app_base {:#x} is outside the writable window {:#x}..{:#x}",
                board.flash.app_base, board.flash.writable_start, board.flash.writable_end
            )));
        }

        exists(manifest, &board.app.file, name, "app.file", path)?;
        if let Some(remedy) = &board.remedy.softdevice {
            exists(
                manifest,
                &remedy.payload.file,
                name,
                "remedy.softdevice.file",
                path,
            )?;
            // Clause 2, enforced: the notice travels with the distribution.
            exists(
                manifest,
                &remedy.license,
                name,
                "remedy.softdevice.license",
                path,
            )?;
        }
        // A precondition without a remedy is a dead end for the user. It is
        // allowed — "I cannot fix this, here is why" beats writing anyway —
        // but a remedy without the precondition it repairs is nonsense.
        if board.remedy.softdevice.is_some() && board.requires.softdevice.is_none() {
            return Err(bad(format!(
                "board {name}: a softdevice remedy with no requires.softdevice to trigger it"
            )));
        }
    }
    Ok(())
}

fn exists(
    manifest: &Manifest,
    file: &Path,
    board: &str,
    field: &str,
    path: &Path,
) -> Result<(), Error> {
    if file.is_absolute() || file.components().any(|c| c.as_os_str() == "..") {
        return Err(Error::Invalid {
            path: path.to_path_buf(),
            message: format!("board {board}: {field} {file:?} must stay inside the bundle"),
        });
    }
    if manifest.root.join(file).is_file() {
        return Ok(());
    }
    Err(Error::Invalid {
        path: path.to_path_buf(),
        message: format!("board {board}: {field} {file:?} is not in the bundle"),
    })
}

/// Find the bundle: `--bundle`, then `$LNFLASH_BUNDLE`, then next to the
/// executable, then the system path. Returns the directory holding
/// `manifest.toml`.
///
/// Each candidate is accepted both as the directory containing the manifest
/// and as the unpacked tarball root containing `firmware/`, because a user
/// who points `--bundle` at the directory they just `cd`'d into is right.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf, Error> {
    let mut tried = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = explicit {
        candidates.push(path.to_path_buf());
    }
    if let Some(from_env) = std::env::var_os(BUNDLE_ENV) {
        candidates.push(PathBuf::from(from_env));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
        }
    }
    candidates.push(PathBuf::from(SYSTEM_BUNDLE));

    for candidate in candidates {
        for dir in [candidate.clone(), candidate.join(FIRMWARE_DIR)] {
            if dir.join(MANIFEST_NAME).is_file() {
                return Ok(dir);
            }
            tried.push(dir.display().to_string());
        }
    }
    Err(Error::NoBundle(tried))
}

pub fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A bundle shaped exactly like the real one, with stand-in payloads so
    /// the test states its own checksums.
    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            fs::create_dir_all(dir.path().join("t114")).unwrap();
            let f = Self { dir };
            f.write("t114/leviculum-t114-0.8.0.uf2", b"application image");
            f.write("t114/s140_nrf52_7.3.0_softdevice.hex", b":00000001FF\n");
            f.write(
                "t114/s140_nrf52_7.3.0_license-agreement.txt",
                b"Copyright (c) Nordic Semiconductor ASA",
            );
            f.write_manifest(&f.manifest_text());
            f
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            fs::write(self.dir.path().join(rel), bytes).unwrap();
        }

        fn write_manifest(&self, text: &str) {
            self.write(MANIFEST_NAME, text.as_bytes());
        }

        fn sha(&self, rel: &str) -> String {
            hex_digest(&fs::read(self.dir.path().join(rel)).unwrap())
        }

        fn manifest_text(&self) -> String {
            format!(
                r#"
[bundle]
version = "0.8.0"
built = "2026-08-10"

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
sha256  = "{app}"
git_sha = "bb7c4f64"

[board.t114.requires]
softdevice = ">=7.0.1, <8.0.0"

[board.t114.remedy.softdevice]
file    = "t114/s140_nrf52_7.3.0_softdevice.hex"
sha256  = "{sd}"
license = "t114/s140_nrf52_7.3.0_license-agreement.txt"
convert = "hex-to-uf2"
"#,
                app = self.sha("t114/leviculum-t114-0.8.0.uf2"),
                sd = self.sha("t114/s140_nrf52_7.3.0_softdevice.hex"),
            )
        }

        fn load(&self) -> Result<Manifest, Error> {
            load(self.dir.path())
        }
    }

    #[test]
    fn a_well_formed_bundle_loads_with_every_axis_populated() {
        let f = Fixture::new();
        let manifest = f.load().unwrap();
        assert_eq!(manifest.bundle.version, "0.8.0");
        let board = manifest.board("t114").unwrap();
        assert_eq!(board.transport, Transport::Uf2Msc);
        assert_eq!(board.entry, vec![Entry::Touch1200, Entry::DoubleTap]);
        assert_eq!(board.identify.info_uf2_board_id, "HT-n5262");
        assert_eq!(board.flash.family_id, crate::uf2::FAMILY_NRF52840_APP);
        assert_eq!(board.flash.app_base, 0x2_7000);
        assert_eq!(board.app.git_sha.as_deref(), Some("bb7c4f64"));
        assert_eq!(
            board.softdevice_req("t114").unwrap().unwrap().as_str(),
            ">=7.0.1, <8.0.0"
        );
        assert_eq!(
            board.remedy.softdevice.as_ref().unwrap().payload.convert,
            Some(Convert::HexToUf2)
        );
    }

    #[test]
    fn usb_ids_in_the_manifest_are_parsed_not_matched_as_strings() {
        let f = Fixture::new();
        let manifest = f.load().unwrap();
        let board = manifest.board("t114").unwrap();
        assert_eq!(
            board.bootloader_ids("t114").unwrap(),
            vec!["239a:0071".parse::<UsbId>().unwrap()]
        );
        assert_eq!(board.candidate_ids("t114").unwrap().len(), 2);
    }

    #[test]
    fn payloads_verify_and_the_whole_bundle_verifies() {
        let f = Fixture::new();
        let manifest = f.load().unwrap();
        let board = manifest.board("t114").unwrap();
        assert_eq!(
            board.app.read(&manifest.root).unwrap(),
            b"application image"
        );
        manifest.verify_all().unwrap();
    }

    #[test]
    fn a_payload_whose_bytes_changed_cannot_be_read_at_all() {
        let f = Fixture::new();
        let manifest = f.load().unwrap();
        f.write("t114/leviculum-t114-0.8.0.uf2", b"application imagX");
        let err = manifest.board("t114").unwrap().app.read(&manifest.root);
        assert!(matches!(err, Err(Error::Checksum { .. })), "{err:?}");
        assert!(matches!(manifest.verify_all(), Err(Error::Checksum { .. })));
    }

    #[test]
    fn a_board_the_bundle_does_not_carry_is_named_along_with_the_ones_it_does() {
        let f = Fixture::new();
        let manifest = f.load().unwrap();
        match manifest.board("rak4631") {
            Err(Error::UnknownBoard { wanted, available }) => {
                assert_eq!(wanted, "rak4631");
                assert_eq!(available, vec!["t114".to_string()]);
            }
            other => panic!("expected UnknownBoard, got {other:?}"),
        }
        assert_eq!(manifest.names(), vec!["t114"]);
    }

    #[test]
    fn a_board_is_looked_up_by_the_id_the_bootloader_published() {
        let f = Fixture::new();
        let manifest = f.load().unwrap();
        assert_eq!(manifest.board_for_id("HT-n5262").unwrap().0, "t114");
        // Exactly, never as a substring: the RAK's ID must not match.
        assert!(manifest.board_for_id("HT-n5262-something").is_none());
        assert!(manifest.board_for_id("WisBlock-RAK4631-Board").is_none());
    }

    #[test]
    fn a_remedy_without_its_licence_file_will_not_load() {
        let f = Fixture::new();
        fs::remove_file(
            f.dir
                .path()
                .join("t114/s140_nrf52_7.3.0_license-agreement.txt"),
        )
        .unwrap();
        let err = f.load().unwrap_err();
        assert!(
            format!("{err}").contains("s140_nrf52_7.3.0_license-agreement.txt"),
            "the licence must be named in the error: {err}"
        );
    }

    #[test]
    fn a_payload_file_missing_from_the_bundle_will_not_load() {
        let f = Fixture::new();
        fs::remove_file(f.dir.path().join("t114/leviculum-t114-0.8.0.uf2")).unwrap();
        assert!(matches!(f.load(), Err(Error::Invalid { .. })));
    }

    #[test]
    fn a_payload_path_may_not_escape_the_bundle() {
        let f = Fixture::new();
        f.write_manifest(&f.manifest_text().replace(
            "\"t114/s140_nrf52_7.3.0_license-agreement.txt\"",
            "\"../../../etc/passwd\"",
        ));
        let err = f.load().unwrap_err();
        assert!(
            format!("{err}").contains("must stay inside the bundle"),
            "{err}"
        );
    }

    #[test]
    fn a_manifest_naming_the_bootloader_family_will_not_load() {
        let f = Fixture::new();
        f.write_manifest(&f.manifest_text().replace("0xADA52840", "0xD663823C"));
        let err = f.load().unwrap_err();
        assert!(format!("{err}").contains("bootloader family"), "{err}");
    }

    #[test]
    fn an_app_base_outside_the_writable_window_will_not_load() {
        let f = Fixture::new();
        // 0xEC000 is the identity page, above what the bootloader will write.
        f.write_manifest(
            &f.manifest_text()
                .replace("app_base       = 0x27000", "app_base = 0xEC000"),
        );
        let err = f.load().unwrap_err();
        assert!(
            format!("{err}").contains("outside the writable window"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_board_id_will_not_load_because_nothing_could_confirm_it() {
        let f = Fixture::new();
        f.write_manifest(
            &f.manifest_text()
                .replace("\"HT-n5262\"\nbootloader_usb", "\"\"\nbootloader_usb"),
        );
        let err = f.load().unwrap_err();
        assert!(
            format!("{err}").contains("info_uf2_board_id is empty"),
            "{err}"
        );
    }

    #[test]
    fn a_board_with_no_entry_mechanism_will_not_load() {
        let f = Fixture::new();
        f.write_manifest(&f.manifest_text().replace(
            r#"entry     = ["touch-1200", "double-tap"]"#,
            "entry     = []",
        ));
        let err = f.load().unwrap_err();
        assert!(format!("{err}").contains("no entry mechanism"), "{err}");
    }

    #[test]
    fn an_unknown_transport_will_not_load() {
        let f = Fixture::new();
        f.write_manifest(&f.manifest_text().replace("uf2-msc", "carrier-pigeon"));
        assert!(matches!(f.load(), Err(Error::Toml { .. })));
    }

    #[test]
    fn a_malformed_version_constraint_will_not_load() {
        let f = Fixture::new();
        f.write_manifest(&f.manifest_text().replace(">=7.0.1, <8.0.0", "7ish"));
        assert!(matches!(f.load(), Err(Error::BadConstraint { .. })));
    }

    #[test]
    fn a_malformed_usb_id_will_not_load() {
        let f = Fixture::new();
        f.write_manifest(&f.manifest_text().replace("239a:0071", "239a-0071"));
        assert!(matches!(f.load(), Err(Error::BadUsbId { .. })));
    }

    #[test]
    fn a_remedy_with_no_precondition_to_trigger_it_will_not_load() {
        let f = Fixture::new();
        f.write_manifest(
            &f.manifest_text()
                .replace("softdevice = \">=7.0.1, <8.0.0\"", ""),
        );
        let err = f.load().unwrap_err();
        assert!(format!("{err}").contains("no requires.softdevice"), "{err}");
    }

    #[test]
    fn the_bundle_is_found_both_as_the_manifest_dir_and_as_the_tarball_root() {
        let f = Fixture::new();
        let found = locate(Some(f.dir.path())).unwrap();
        assert_eq!(found, f.dir.path());

        let outer = TempDir::new().unwrap();
        let inner = outer.path().join(FIRMWARE_DIR);
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join(MANIFEST_NAME), "").unwrap();
        assert_eq!(locate(Some(outer.path())).unwrap(), inner);
    }

    #[test]
    fn a_bundle_that_is_nowhere_names_everywhere_it_looked() {
        let missing = TempDir::new().unwrap().path().join("gone");
        match locate(Some(&missing)) {
            Err(Error::NoBundle(tried)) => {
                assert!(tried.iter().any(|t| t.contains("gone")));
                assert!(tried.iter().any(|t| t.contains(SYSTEM_BUNDLE)));
            }
            other => panic!("expected NoBundle, got {other:?}"),
        }
    }
}
