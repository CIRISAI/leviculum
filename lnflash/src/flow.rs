//! The sequence, and only the sequence.
//!
//! ```text
//! find candidates
//!   -> bring the board into its bootloader
//!   -> CONFIRM IDENTITY THERE, from INFO_UF2.TXT
//!   -> check the SoftDevice precondition
//!   -> install the SoftDevice first if it is not satisfied
//!   -> verify the image checksum
//!   -> write the application
//!   -> verify it booted
//! ```
//!
//! **No write may rest on a guessed identity.** Commit `362c1c2d` records a
//! T114 image landing on a RAK4631 when that rule was absent. It is enforced
//! here by construction rather than by ordering: [`Confirmed`] can only be
//! produced by [`confirm_identity`] from an `INFO_UF2.TXT` read off a
//! mounted bootloader drive, and the write functions take one. There is no
//! way to call them with a board guessed from a USB ID.
//!
//! With several devices attached, each is resolved individually. "The one
//! UF2 drive" is an assumption, and it is the assumption that went wrong.

use std::path::Path;
use std::time::Duration;

use crate::entry;
use crate::infouf2::InfoUf2;
use crate::manifest::{self, Board, Manifest, Payload};
use crate::softdevice::{self, Version, VersionReq};
use crate::transport::{self, Drive, Written};
use crate::uf2::Image;
use crate::ui::Ui;
use crate::usb::{Device, Sysfs, UsbId};
use crate::verify::{self, Verdict};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Manifest(#[from] manifest::Error),
    #[error("{0}")]
    Transport(#[from] transport::Error),
    #[error("{0}")]
    Uf2(#[from] crate::uf2::Error),
    #[error("{0}")]
    Ihex(#[from] crate::ihex::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error(
        "the bootloader on {port} published no Board-ID, so there is nothing to confirm and \
         nothing may be written to it"
    )]
    NoBoardId { port: String },
    #[error(
        "the board on {port} says it is {board_id:?}, which this bundle does not carry \
         (it carries {}). Nothing was written.",
        available.join(", ")
    )]
    UnknownBoard {
        port: String,
        board_id: String,
        available: Vec<String>,
    },
    #[error(
        "asked to flash {asked}, but the board on {port} says it is {board_id:?} ({found}). \
         Nothing was written."
    )]
    WrongBoard {
        port: String,
        asked: String,
        found: String,
        board_id: String,
    },
    #[error("the board on {port} never appeared in its bootloader")]
    NoBootloader { port: String },
    #[error(
        "{file}: this image is family {actual:#010x}, and {board} takes {expected:#010x}. \
         Nothing was written."
    )]
    WrongFamily {
        file: String,
        board: String,
        actual: u32,
        expected: u32,
    },
    #[error(
        "{file}: covers {low:#x}-{high:#x}, which leaves the writable window \
         {start:#x}-{end:#x} that {board}'s bootloader accepts. Nothing was written."
    )]
    OutsideWindow {
        file: String,
        board: String,
        low: u32,
        high: u32,
        start: u32,
        end: u32,
    },
    #[error(
        "{board} needs SoftDevice {req} and the board has {found}, but this bundle carries no \
         remedy for that. Nothing was written."
    )]
    NoRemedy {
        board: String,
        req: String,
        found: String,
    },
    #[error("cancelled")]
    Cancelled,
}

/// A board identity read off a mounted bootloader drive.
///
/// The private field is the whole point: this cannot be constructed from a
/// USB ID, a command-line flag, or a guess. Only [`confirm_identity`] makes
/// one, and it only does so from an `INFO_UF2.TXT`.
#[derive(Debug, Clone, Copy)]
pub struct Confirmed<'m> {
    name: &'m str,
    board: &'m Board,
    _private: (),
}

impl<'m> Confirmed<'m> {
    pub fn name(&self) -> &'m str {
        self.name
    }

    pub fn board(&self) -> &'m Board {
        self.board
    }
}

/// Stage two of identify: match what the bootloader published against the
/// bundle. Exact match, never a substring — the T114's `Board-ID` is exactly
/// `HT-n5262`, and a substring rule is how a near-miss becomes a wrong write.
pub fn confirm_identity<'m>(
    manifest: &'m Manifest,
    info: &InfoUf2,
    port: &str,
    asked_for: Option<&str>,
) -> Result<Confirmed<'m>, Error> {
    let board_id = info.board_id().map(str::trim).filter(|id| !id.is_empty());
    let Some(board_id) = board_id else {
        return Err(Error::NoBoardId {
            port: port.to_string(),
        });
    };
    let Some((name, board)) = manifest.board_for_id(board_id) else {
        return Err(Error::UnknownBoard {
            port: port.to_string(),
            board_id: board_id.to_string(),
            available: manifest.names().iter().map(|s| s.to_string()).collect(),
        });
    };
    if let Some(asked) = asked_for {
        if asked != name {
            return Err(Error::WrongBoard {
                port: port.to_string(),
                asked: asked.to_string(),
                found: name.to_string(),
                board_id: board_id.to_string(),
            });
        }
    }
    Ok(Confirmed {
        name,
        board,
        _private: (),
    })
}

/// What the two independent readings of the SoftDevice version say together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// From the bootloader's `SoftDevice:` line. Absent on older bootloaders.
    pub from_info: Option<Version>,
    /// From the version word at `0x3014` in the flash dump. Absent if the
    /// dump does not reach it.
    pub from_flash: Option<Version>,
}

impl Installed {
    /// The version to decide on. The flash word is preferred: it is the
    /// SoftDevice's own statement, and it exists on bootloaders too old to
    /// emit the line.
    pub fn version(&self) -> Option<Version> {
        self.from_flash.or(self.from_info)
    }

    /// Whether the two readings disagree. They agreed on both rig boards; a
    /// disagreement is worth telling the user about rather than silently
    /// preferring one.
    pub fn disagree(&self) -> bool {
        matches!((self.from_info, self.from_flash), (Some(a), Some(b)) if a != b)
    }

    pub fn describe(&self) -> String {
        match (self.from_info, self.from_flash) {
            (Some(a), Some(b)) if a == b => format!("{a} (bootloader and flash agree)"),
            (Some(a), Some(b)) => format!("{b} in flash, but the bootloader reports {a}"),
            (None, Some(b)) => format!("{b} (read from flash; the bootloader does not report it)"),
            (Some(a), None) => format!("{a} (the bootloader's word for it)"),
            (None, None) => "unknown".to_string(),
        }
    }
}

/// Where a precondition stands, and what follows from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// No constraint stated, or the constraint holds.
    Met,
    /// The constraint does not hold and a remedy has to run first.
    NeedsRemedy { found: String, req: String },
    /// The constraint does not hold and nothing can be read to decide it.
    /// Treated as "needs the remedy": writing our application onto a board
    /// carrying 6.1.1 produces a device that goes dark, and the cost of
    /// installing a SoftDevice that was already fine is one rewrite of
    /// identical bytes.
    Unknown { req: String },
}

pub fn check_softdevice(installed: &Installed, req: Option<&VersionReq>) -> Precondition {
    let Some(req) = req else {
        return Precondition::Met;
    };
    match installed.version() {
        Some(version) if req.matches(version) => Precondition::Met,
        Some(version) => Precondition::NeedsRemedy {
            found: version.to_string(),
            req: req.as_str().to_string(),
        },
        None => Precondition::Unknown {
            req: req.as_str().to_string(),
        },
    }
}

/// Read both statements of the installed SoftDevice version.
pub fn read_installed(drive: &Drive, info: &InfoUf2) -> Installed {
    Installed {
        from_info: info.softdevice().map(|sd| sd.version),
        from_flash: drive
            .current()
            .ok()
            .and_then(|dump| softdevice::installed_version(&dump)),
    }
}

/// Turn a payload into the image that will be written, and check it against
/// what the board's bootloader will accept.
///
/// Every refusal here happens before anything is mounted for writing, so
/// "nothing was written" in the error text is true.
pub fn prepare(payload: &Payload, root: &Path, confirmed: &Confirmed) -> Result<Image, Error> {
    // The only way to get the bytes, and it verifies the checksum.
    let bytes = payload.read(root)?;
    let board = confirmed.board();
    let image = match payload.convert.unwrap_or(manifest::Convert::None) {
        manifest::Convert::None => Image::parse(&bytes)?,
        manifest::Convert::HexToUf2 => {
            let text = String::from_utf8_lossy(&bytes);
            Image::from_spans(&crate::ihex::parse(&text)?, board.flash.family_id)
        }
    };
    let file = payload.file.display().to_string();

    if let Some(actual) = image.family_id() {
        if actual != board.flash.family_id {
            return Err(Error::WrongFamily {
                file,
                board: confirmed.name().to_string(),
                actual,
                expected: board.flash.family_id,
            });
        }
    }
    // The upper bound is the one that matters: at or above `writable_end`
    // the bootloader rejects outright. Below `writable_start` blocks are
    // declined silently, which is expected for a SoftDevice image carrying
    // an MBR, so a low start is reported, not refused.
    let (low, high) = image.address_range().unwrap_or((0, 0));
    if high > board.flash.writable_end {
        return Err(Error::OutsideWindow {
            file,
            board: confirmed.name().to_string(),
            low,
            high,
            start: board.flash.writable_start,
            end: board.flash.writable_end,
        });
    }
    Ok(image)
}

/// One line describing what a prepared image will do to the board.
pub fn describe_image(label: &str, image: &Image, writable_start: u32) -> String {
    let (low, high) = image.address_range().unwrap_or((0, 0));
    let declined = image.blocks_below(writable_start);
    let tail = if declined > 0 {
        format!(
            ", of which {declined} below {writable_start:#x} are declined by the bootloader \
             (the MBR; harmless, and they never land)"
        )
    } else {
        String::new()
    };
    format!(
        "  {label}: {low:#x}-{high:#x}, {} blocks{tail}",
        image.blocks.len()
    )
}

/// How the tool was asked to behave.
#[derive(Debug, Clone)]
pub struct Options {
    /// Only touch this board, and refuse if what is attached is another one.
    pub board: Option<String>,
    /// Inspect and report; change nothing, and do not even reboot a board
    /// into its bootloader, which is itself a change to somebody's device.
    pub dry_run: bool,
    /// How long to wait for a bootloader or an application to appear.
    pub appear_within: Duration,
    /// How long to listen for the `[FW_BUILD]` banner.
    pub banner_window: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            board: None,
            dry_run: false,
            appear_within: entry::BOOTLOADER_APPEARS_WITHIN,
            banner_window: verify::BANNER_WINDOW,
        }
    }
}

/// A device worth looking at, and why.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub device: Device,
    /// True when the USB ID is one a bootloader answers on. A hint about
    /// what to do next, never about what the board is.
    pub in_bootloader: bool,
    /// The board whose manifest entry listed this USB ID. A hint only: the
    /// USB ID of a running application belongs to its firmware.
    pub hint: String,
}

/// Everything on the bus this bundle has any business touching.
pub fn find_candidates(manifest: &Manifest, sysfs: &Sysfs) -> Result<Vec<Candidate>, Error> {
    let devices = sysfs.devices()?;
    let mut out: Vec<Candidate> = Vec::new();
    for (name, board) in &manifest.board {
        let bootloader: Vec<UsbId> = board.bootloader_ids(name)?;
        let application: Vec<UsbId> = board.candidate_ids(name)?;
        for device in &devices {
            let in_bootloader = bootloader.contains(&device.id);
            if !in_bootloader && !application.contains(&device.id) {
                continue;
            }
            if out.iter().any(|c| c.device.name == device.name) {
                continue;
            }
            out.push(Candidate {
                device: device.clone(),
                in_bootloader,
                hint: name.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.device.name.cmp(&b.device.name));
    Ok(out)
}

impl Candidate {
    pub fn describe(&self) -> String {
        let serial = self.device.serial.as_deref().unwrap_or("no serial");
        let what = if self.in_bootloader {
            format!("in its bootloader — probably a {}", self.hint)
        } else {
            format!(
                "running {} — probably a {}",
                self.device.product.as_deref().unwrap_or("unknown firmware"),
                self.hint
            )
        };
        format!(
            "  {} [{}] {serial}: {what}",
            self.device.name, self.device.id
        )
    }
}

/// What happened to one board.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub port: String,
    pub board: String,
    pub softdevice_installed: Option<Written>,
    pub application_written: Option<Written>,
    pub verdict: Option<Verdict>,
}

impl Outcome {
    pub fn is_good(&self) -> bool {
        self.application_written.is_some()
            && self.verdict.as_ref().is_some_and(Verdict::is_confirmed)
    }
}

/// Resolve every candidate on the bus, individually.
pub fn run(
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
) -> Result<Vec<Outcome>, Error> {
    let candidates = find_candidates(manifest, sysfs)?;
    if candidates.is_empty() {
        ui.say("No board this bundle knows is attached.");
        ui.say(&format!(
            "It carries: {}. Nothing to do.",
            manifest.names().join(", ")
        ));
        return Ok(Vec::new());
    }

    ui.say(&format!("Found {} device(s):", candidates.len()));
    for candidate in &candidates {
        ui.say(&candidate.describe());
    }
    ui.say("");

    let mut outcomes = Vec::new();
    for candidate in candidates {
        match resolve(manifest, sysfs, ui, opts, &candidate) {
            Ok(Some(outcome)) => outcomes.push(outcome),
            Ok(None) => {}
            // One board's refusal must not abandon the others: with several
            // devices attached each is its own decision.
            Err(err) => ui.say(&format!("{}: {err}\n", candidate.device.name)),
        }
    }
    Ok(outcomes)
}

fn resolve(
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    candidate: &Candidate,
) -> Result<Option<Outcome>, Error> {
    let port = candidate.device.name.clone();

    if opts.dry_run && !candidate.in_bootloader {
        ui.say(&format!(
            "{port}: would enter the bootloader and confirm what it is there. \
             --dry-run stops here, because rebooting a board is already a change to it."
        ));
        return Ok(None);
    }
    if opts.dry_run && !crate::sys::is_root() {
        // Reading INFO_UF2.TXT means mounting, and mounting means root. Say
        // what is missing rather than reporting the board as a problem.
        ui.say(&format!(
            "{port}: already in its bootloader. Reading INFO_UF2.TXT off it needs root, so \
             run --dry-run under sudo to see the identity and SoftDevice check too."
        ));
        return Ok(None);
    }

    let bootloader = if candidate.in_bootloader {
        candidate.device.clone()
    } else {
        enter_bootloader(manifest, sysfs, ui, opts, candidate)?
    };
    let Some(block) = bootloader.block_device() else {
        return Err(transport::Error::NoDrive { port }.into());
    };

    let drive = Drive::open(&block)?;
    let info = drive.info()?;
    let confirmed = confirm_identity(manifest, &info, &port, opts.board.as_deref())?;
    ui.say(&format!(
        "{port}: confirmed {} — Board-ID {:?}, bootloader {}",
        confirmed.name(),
        info.board_id().unwrap_or(""),
        info.banner.as_deref().unwrap_or("unreported")
    ));

    let installed = read_installed(&drive, &info);
    ui.say(&format!("{port}: SoftDevice {}", installed.describe()));
    if installed.disagree() {
        ui.say(&format!(
            "{port}: the bootloader's SoftDevice line and the version word in flash disagree. \
             Going by the flash, which is the SoftDevice's own statement."
        ));
    }

    let req = confirmed.board().softdevice_req(confirmed.name())?;
    let precondition = check_softdevice(&installed, req.as_ref());
    let app_image = prepare(&confirmed.board().app, &manifest.root, &confirmed)?;

    // Everything that could refuse has refused by now, so the plan the user
    // is shown is the plan that will run.
    let mut plan: Vec<String> = Vec::new();
    let remedy_image = match &precondition {
        Precondition::Met => None,
        needs => {
            let Some(remedy) = &confirmed.board().remedy.softdevice else {
                return Err(Error::NoRemedy {
                    board: confirmed.name().to_string(),
                    req: req.map(|r| r.as_str().to_string()).unwrap_or_default(),
                    found: installed.describe(),
                });
            };
            plan.push(match needs {
                Precondition::NeedsRemedy { found, req } => format!(
                    "  install SoftDevice {} first — the board has {found}, and {} needs {req}. \
                     Writing our application onto the wrong one produces a board that goes dark.",
                    remedy.payload.file.display(),
                    confirmed.name()
                ),
                _ => format!(
                    "  install SoftDevice {} first — the installed version cannot be read, and \
                     guessing wrong produces a board that goes dark.",
                    remedy.payload.file.display()
                ),
            });
            let image = prepare(&remedy.payload, &manifest.root, &confirmed)?;
            plan.push(describe_image(
                "which writes",
                &image,
                confirmed.board().flash.writable_start,
            ));
            plan.push(format!(
                "  its licence, {}, ships with it",
                remedy.license.display()
            ));
            Some(image)
        }
    };
    plan.push(format!(
        "  write {} ({})",
        confirmed.board().app.file.display(),
        confirmed
            .board()
            .app
            .git_sha
            .as_deref()
            .map(|sha| format!("git_sha={sha}"))
            .unwrap_or_else(|| "no build recorded".into())
    ));
    plan.push(describe_image(
        "which writes",
        &app_image,
        confirmed.board().flash.writable_start,
    ));

    ui.say(&format!("\n{port}: this will"));
    for line in &plan {
        ui.say(line);
    }

    if opts.dry_run {
        ui.say(&format!("{port}: --dry-run, so nothing was written.\n"));
        return Ok(None);
    }
    if !ui.confirm(&format!(
        "\n{port}: overwrite the firmware on this {}?",
        confirmed.name()
    ))? {
        return Err(Error::Cancelled);
    }

    let mut outcome = Outcome {
        port: port.clone(),
        board: confirmed.name().to_string(),
        softdevice_installed: None,
        application_written: None,
        verdict: None,
    };

    let mut drive = drive;
    if let Some(image) = remedy_image {
        let declined = image.blocks_below(confirmed.board().flash.writable_start);
        let written = drive.write_image("SD.UF2", &image, declined)?;
        report_write(ui, &port, "SoftDevice", &written);
        drive.close()?;
        outcome.softdevice_installed = Some(written);

        // The board reboots on the last block. An application that was
        // already installed boots straight away — it was intact all along —
        // so getting back to the bootloader may need another touch.
        let again = back_to_bootloader(manifest, sysfs, ui, opts, &bootloader, confirmed.name())?;
        let block = again
            .block_device()
            .ok_or_else(|| transport::Error::NoDrive { port: port.clone() })?;
        drive = Drive::open(&block)?;
        // Identity is confirmed again rather than carried over: this is a
        // fresh mount of a drive that reappeared, and the rule is the rule.
        let info = drive.info()?;
        confirm_identity(manifest, &info, &port, Some(confirmed.name()))?;
        let after = read_installed(&drive, &info);
        ui.say(&format!("{port}: SoftDevice now {}", after.describe()));
    }

    let declined = app_image.blocks_below(confirmed.board().flash.writable_start);
    let written = drive.write_image("APP.UF2", &app_image, declined)?;
    report_write(ui, &port, "application", &written);
    drive.close()?;
    outcome.application_written = Some(written);

    outcome.verdict = Some(verify_boot(
        sysfs,
        ui,
        opts,
        &bootloader,
        &confirmed,
        &port,
    )?);
    Ok(Some(outcome))
}

fn report_write(ui: &mut dyn Ui, port: &str, what: &str, written: &Written) {
    let mut line = format!(
        "{port}: copied {} ({} bytes, {} blocks",
        what, written.bytes, written.blocks
    );
    if written.declined > 0 {
        line.push_str(&format!(", {} of them declined", written.declined));
    }
    line.push(')');
    ui.say(&line);
    if written.reboot_error.is_some() {
        ui.say(&format!(
            "{port}: the drive went away mid-flush, which is what a bootloader rebooting on the \
             last block looks like — not a failure"
        ));
    }
}

fn enter_bootloader(
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    candidate: &Candidate,
) -> Result<Device, Error> {
    // The hint is enough to choose *how to knock*; it is not enough to
    // choose what to write, which is why identity is confirmed afterwards.
    let board = manifest.board(&candidate.hint)?;
    let port = candidate.device.name.clone();

    for mechanism in &board.entry {
        match mechanism {
            manifest::Entry::Touch1200 => {
                let Some(tty) = candidate.device.tty(0) else {
                    continue;
                };
                ui.say(&format!(
                    "{port}: 1200-baud touch on {} — the bootloader takes about 5 s to appear",
                    tty.display()
                ));
                if let Err(err) = entry::touch_1200(&tty) {
                    ui.say(&format!("{port}: the touch did not go through ({err})"));
                    continue;
                }
            }
            manifest::Entry::DoubleTap => {
                ui.say(&format!(
                    "{port}: no software trigger worked, so this one needs hands."
                ));
                ui.wait_for_human(&entry::double_tap_instruction(&format!(
                    "The board on {port}"
                )))?;
            }
        }
        let ids = board.bootloader_ids(&candidate.hint)?;
        if let Some(found) =
            entry::wait_for_bootloader(sysfs, &ids, Some(&candidate.device), opts.appear_within)?
        {
            return Ok(found);
        }
        ui.say(&format!("{port}: nothing appeared."));
    }
    Err(Error::NoBootloader { port })
}

/// Get back into the bootloader after a SoftDevice install, which may have
/// left the board running an application that was intact all along.
fn back_to_bootloader(
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    was: &Device,
    board_name: &str,
) -> Result<Device, Error> {
    let board = manifest.board(board_name)?;
    let ids = board.bootloader_ids(board_name)?;
    // Let the drive we just wrote go away first. Without this the wait below
    // answers instantly with the pre-reboot sysfs entry and the next mount
    // lands on a device in the middle of detaching.
    entry::wait_until_gone(sysfs, was, opts.appear_within)?;
    if let Some(found) = entry::wait_for_bootloader(sysfs, &ids, Some(was), opts.appear_within)? {
        return Ok(found);
    }
    let candidate = Candidate {
        device: application_after(sysfs, board, board_name, was, opts)?.unwrap_or(was.clone()),
        in_bootloader: false,
        hint: board_name.to_string(),
    };
    enter_bootloader(manifest, sysfs, ui, opts, &candidate)
}

fn application_after(
    sysfs: &Sysfs,
    board: &Board,
    name: &str,
    was: &Device,
    opts: &Options,
) -> Result<Option<Device>, Error> {
    let ids = board.candidate_ids(name)?;
    Ok(entry::wait_for_application(
        sysfs,
        &ids,
        Some(was),
        opts.appear_within,
    )?)
}

fn verify_boot(
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    was: &Device,
    confirmed: &Confirmed,
    port: &str,
) -> Result<Verdict, Error> {
    let board = confirmed.board();
    let ids = board.candidate_ids(confirmed.name())?;
    // The bootloader drive going away is the first half of "it took"; the
    // application coming back is the second. Waiting for the first also
    // keeps a stale sysfs entry from answering the second.
    entry::wait_until_gone(sysfs, was, opts.appear_within)?;
    let Some(app) = entry::wait_for_application(sysfs, &ids, Some(was), opts.appear_within)? else {
        ui.say(&format!("{port}: the application never came back."));
        return Ok(Verdict::Absent);
    };
    ui.say(&format!(
        "{port}: back as {} [{}]",
        app.product.as_deref().unwrap_or("an application"),
        app.id
    ));

    // if00 is the debug log. Without DTR+RTS it reads as silent even on a
    // healthy board, which `Fd::set_debug_port` asserts.
    let banner = match app.tty(0) {
        Some(tty) => verify::read_banner(&tty, opts.banner_window).unwrap_or(None),
        None => None,
    };
    let verdict = verify::judge(banner, board.app.git_sha.as_deref());
    match &verdict {
        Verdict::Confirmed { git_sha } => {
            ui.say(&format!("{port}: running git_sha={git_sha}. Done."))
        }
        Verdict::WrongBuild { saw, expected } => ui.say(&format!(
            "{port}: the board reports git_sha={saw}, not {expected}. The write did not take."
        )),
        Verdict::Unconfirmed { why } => ui.say(&format!(
            "{port}: re-enumerated, but the firmware could not be confirmed — {why}."
        )),
        Verdict::Absent => {}
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infouf2;
    use crate::manifest::load;
    use std::fs;
    use tempfile::TempDir;

    const T114_INFO: &str = "UF2 Bootloader 0.9.0-2-g836c8dc-dirty\r\n\
                             Model: HT-n5262\r\n\
                             Board-ID: HT-n5262\r\n\
                             Date: Jul  9 2024\r\n\
                             SoftDevice: S140 7.3.0\r\n";

    const RAK_INFO: &str = "UF2 Bootloader 0.4.3\r\n\
                            Model: WisBlock RAK4631 Board\r\n\
                            Board-ID: WisBlock-RAK4631-Board\r\n\
                            Date: May 20 2023\r\n\
                            Ver: 0.4.3\r\n\
                            SoftDevice: S140 7.3.0\r\n";

    /// A one-board bundle, with the real vendored SoftDevice hex so the
    /// remedy path is prepared from the real image.
    fn bundle() -> (TempDir, Manifest) {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("t114")).unwrap();
        let hex = include_str!("../payload/t114/s140_nrf52_7.3.0_softdevice.hex");
        fs::write(dir.path().join("t114/sd.hex"), hex).unwrap();
        fs::write(
            dir.path().join("t114/LICENSE-NORDIC"),
            "Nordic Semiconductor ASA",
        )
        .unwrap();

        let app = Image::from_spans(
            &[crate::ihex::Span {
                start: 0x2_7000,
                data: vec![0xAB; 0x1000],
            }],
            crate::uf2::FAMILY_NRF52840_APP,
        )
        .encode()
        .unwrap();
        fs::write(dir.path().join("t114/app.uf2"), &app).unwrap();

        let text = format!(
            r#"
[bundle]
version = "0.8.0"

[board.t114]
family    = "nrf52840"
transport = "uf2-msc"
entry     = ["touch-1200", "double-tap"]

[board.t114.identify]
info_uf2_board_id = "HT-n5262"
bootloader_usb    = ["239a:0071"]
candidate_usb     = ["1209:0001", "239a:8071"]

[board.t114.flash]
family_id      = 0xADA52840
writable_start = 0x1000
writable_end   = 0xEA000
app_base       = 0x27000

[board.t114.app]
file    = "t114/app.uf2"
sha256  = "{app_sha}"
git_sha = "bb7c4f64"

[board.t114.requires]
softdevice = ">=7.0.1, <8.0.0"

[board.t114.remedy.softdevice]
file    = "t114/sd.hex"
sha256  = "{sd_sha}"
license = "t114/LICENSE-NORDIC"
convert = "hex-to-uf2"
"#,
            app_sha = manifest::hex_digest(&app),
            sd_sha = manifest::hex_digest(hex.as_bytes()),
        );
        fs::write(dir.path().join("manifest.toml"), text).unwrap();
        let manifest = load(dir.path()).unwrap();
        (dir, manifest)
    }

    fn sysfs() -> Sysfs {
        Sysfs::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"))
    }

    #[test]
    fn the_board_the_bootloader_names_is_the_board_that_gets_confirmed() {
        let (_dir, manifest) = bundle();
        let confirmed =
            confirm_identity(&manifest, &infouf2::parse(T114_INFO), "3-2.4", None).unwrap();
        assert_eq!(confirmed.name(), "t114");
        assert_eq!(confirmed.board().flash.app_base, 0x2_7000);
    }

    #[test]
    fn a_rak_in_the_bootloader_is_refused_by_a_t114_only_bundle() {
        // The 362c1c2d failure, in one assertion: a T114 image must not land
        // on a RAK because the drive looked the same.
        let (_dir, manifest) = bundle();
        let err =
            confirm_identity(&manifest, &infouf2::parse(RAK_INFO), "3-2.4", None).unwrap_err();
        assert!(matches!(err, Error::UnknownBoard { .. }));
        assert!(format!("{err}").contains("Nothing was written"));
        assert!(format!("{err}").contains("WisBlock-RAK4631-Board"));
    }

    #[test]
    fn asking_for_one_board_and_finding_another_refuses_rather_than_flashing_it() {
        let (_dir, manifest) = bundle();
        // `--board rak4631` with a T114 on the bench: the bootloader says
        // T114, so the run stops rather than writing what was asked for.
        let err = confirm_identity(
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            Some("rak4631"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::WrongBoard { .. }));
        assert!(format!("{err}").contains("Nothing was written"));
        // ...and asking for the board that is actually there is fine.
        assert_eq!(
            confirm_identity(&manifest, &infouf2::parse(T114_INFO), "3-2.4", Some("t114"))
                .unwrap()
                .name(),
            "t114"
        );
    }

    #[test]
    fn a_bootloader_that_publishes_no_board_id_gets_nothing_written_to_it() {
        let (_dir, manifest) = bundle();
        let info = infouf2::parse("UF2 Bootloader 0.9.0\r\nDate: Jul  9 2024\r\n");
        assert!(matches!(
            confirm_identity(&manifest, &info, "3-2.4", None),
            Err(Error::NoBoardId { .. })
        ));
        // An empty value is the same as no value: it confirms nothing.
        let blank = infouf2::parse("Board-ID:  \r\n");
        assert!(matches!(
            confirm_identity(&manifest, &blank, "3-2.4", None),
            Err(Error::NoBoardId { .. })
        ));
    }

    #[test]
    fn the_precondition_is_met_when_the_board_carries_a_seven() {
        let installed = Installed {
            from_info: Some(Version::new(7, 3, 0)),
            from_flash: Some(Version::new(7, 3, 0)),
        };
        let req = VersionReq::parse(">=7.0.1, <8.0.0").unwrap();
        assert_eq!(check_softdevice(&installed, Some(&req)), Precondition::Met);
        assert!(!installed.disagree());
        assert!(installed.describe().contains("agree"));
    }

    #[test]
    fn a_factory_board_needs_the_remedy_before_anything_is_written() {
        let installed = Installed {
            from_info: Some(Version::new(6, 1, 1)),
            from_flash: Some(Version::new(6, 1, 1)),
        };
        let req = VersionReq::parse(">=7.0.1, <8.0.0").unwrap();
        match check_softdevice(&installed, Some(&req)) {
            Precondition::NeedsRemedy { found, req } => {
                assert_eq!(found, "6.1.1");
                assert_eq!(req, ">=7.0.1, <8.0.0");
            }
            other => panic!("expected NeedsRemedy, got {other:?}"),
        }
    }

    #[test]
    fn the_flash_word_outranks_the_bootloader_line_and_the_disagreement_is_reported() {
        let installed = Installed {
            from_info: Some(Version::new(6, 1, 1)),
            from_flash: Some(Version::new(7, 3, 0)),
        };
        assert_eq!(installed.version(), Some(Version::new(7, 3, 0)));
        assert!(installed.disagree());
        assert!(installed.describe().contains("but the bootloader reports"));
    }

    #[test]
    fn a_bootloader_too_old_to_report_a_version_still_yields_one_from_flash() {
        let installed = Installed {
            from_info: None,
            from_flash: Some(Version::new(7, 3, 0)),
        };
        let req = VersionReq::parse(">=7.0.1, <8.0.0").unwrap();
        assert_eq!(check_softdevice(&installed, Some(&req)), Precondition::Met);
    }

    #[test]
    fn an_unreadable_version_takes_the_remedy_rather_than_the_risk() {
        let installed = Installed {
            from_info: None,
            from_flash: None,
        };
        let req = VersionReq::parse(">=7.0.1, <8.0.0").unwrap();
        assert!(matches!(
            check_softdevice(&installed, Some(&req)),
            Precondition::Unknown { .. }
        ));
        // No constraint at all is a different thing and needs no remedy.
        assert_eq!(check_softdevice(&installed, None), Precondition::Met);
    }

    #[test]
    fn the_remedy_image_is_prepared_from_the_hex_and_fits_the_window() {
        let (_dir, manifest) = bundle();
        let confirmed =
            confirm_identity(&manifest, &infouf2::parse(T114_INFO), "3-2.4", None).unwrap();
        let remedy = &confirmed
            .board()
            .remedy
            .softdevice
            .as_ref()
            .unwrap()
            .payload;
        let image = prepare(remedy, &manifest.root, &confirmed).unwrap();
        assert_eq!(image.blocks.len(), 608);
        assert_eq!(image.address_range(), Some((0x0, 0x2_6500)));
        assert_eq!(image.blocks_below(0x1000), 11);
        assert!(describe_image("which writes", &image, 0x1000).contains("11 below 0x1000"));
    }

    #[test]
    fn a_payload_with_the_wrong_checksum_never_becomes_an_image() {
        let (dir, manifest) = bundle();
        let confirmed =
            confirm_identity(&manifest, &infouf2::parse(T114_INFO), "3-2.4", None).unwrap();
        fs::write(dir.path().join("t114/app.uf2"), b"tampered").unwrap();
        let err = prepare(&confirmed.board().app, &manifest.root, &confirmed).unwrap_err();
        assert!(matches!(
            err,
            Error::Manifest(manifest::Error::Checksum { .. })
        ));
    }

    #[test]
    fn an_image_reaching_past_the_writable_window_is_refused_before_any_mount() {
        let (dir, manifest) = bundle();
        // 0xEC000 is the identity page: above what the bootloader accepts.
        let too_high = Image::from_spans(
            &[crate::ihex::Span {
                start: 0xEB000,
                data: vec![0u8; 0x2000],
            }],
            crate::uf2::FAMILY_NRF52840_APP,
        )
        .encode()
        .unwrap();
        fs::write(dir.path().join("t114/app.uf2"), &too_high).unwrap();
        let text = fs::read_to_string(dir.path().join("manifest.toml"))
            .unwrap()
            .replace(
                &manifest.board("t114").unwrap().app.sha256,
                &manifest::hex_digest(&too_high),
            );
        fs::write(dir.path().join("manifest.toml"), text).unwrap();
        let manifest = load(dir.path()).unwrap();
        let confirmed =
            confirm_identity(&manifest, &infouf2::parse(T114_INFO), "3-2.4", None).unwrap();
        let err = prepare(&confirmed.board().app, &manifest.root, &confirmed).unwrap_err();
        assert!(matches!(err, Error::OutsideWindow { .. }), "{err}");
        assert!(format!("{err}").contains("Nothing was written"));
    }

    #[test]
    fn candidates_are_found_by_usb_id_and_labelled_as_hints_only() {
        let (_dir, manifest) = bundle();
        let found = find_candidates(&manifest, &sysfs()).unwrap();
        let names: Vec<&str> = found.iter().map(|c| c.device.name.as_str()).collect();
        // 3-2.3.1 is our T114 application, 3-2.4 its bootloader. The RAK on
        // 3-2.3.4.4 is 1209:0002, which this bundle does not list.
        assert_eq!(names, vec!["3-2.3.1", "3-2.4"]);
        assert!(!found[0].in_bootloader);
        assert!(found[1].in_bootloader);
        assert!(found[0].describe().contains("probably a t114"));
    }

    #[test]
    fn an_empty_bus_is_reported_rather_than_waited_on() {
        let (_dir, manifest) = bundle();
        let empty = TempDir::new().unwrap();
        let mut ui = crate::ui::testing::Fake::agreeing();
        let outcomes = run(
            &manifest,
            &Sysfs::new(empty.path()),
            &mut ui,
            &Options::default(),
        )
        .unwrap();
        assert!(outcomes.is_empty());
        assert!(ui.transcript().contains("No board this bundle knows"));
        assert!(ui.transcript().contains("t114"));
    }

    #[test]
    fn a_dry_run_will_not_even_reboot_a_board_into_its_bootloader() {
        let (_dir, manifest) = bundle();
        let mut ui = crate::ui::testing::Fake::agreeing();
        let opts = Options {
            dry_run: true,
            ..Options::default()
        };
        // Both candidates are reported; the application-mode one stops
        // before the touch, and the bootloader one stops at Drive::open,
        // which needs root. Neither writes.
        let outcomes = run(&manifest, &sysfs(), &mut ui, &opts).unwrap();
        assert!(outcomes.is_empty());
        let said = ui.transcript();
        assert!(said.contains("would enter the bootloader"));
        assert!(said.contains("rebooting a board is already a change"));
        assert!(ui.answers.len() == 8, "nothing should have been confirmed");
    }
}
