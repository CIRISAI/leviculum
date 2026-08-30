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
//!   -> set the radio configuration it will remember
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

use std::io;
use std::path::Path;
use std::time::Duration;

use crate::entry;
use crate::envelope::SessionReply;
use crate::infouf2::InfoUf2;
use crate::manifest::{self, Board, Catalogue, Manifest, Payload, Payloads};
use crate::radio::{self, RadioChoice, RadioPlan, RadioSettings};
use crate::softdevice::{self, Version, VersionReq};
use crate::telemetry::{self, TelemetryPlan};
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
    payloads: &'m Payloads,
    _private: (),
}

impl<'m> Confirmed<'m> {
    pub fn name(&self) -> &'m str {
        self.name
    }

    /// The hardware facts, from the catalogue.
    pub fn board(&self) -> &'m Board {
        self.board
    }

    /// The images the bundle carries for it. A `Confirmed` cannot exist
    /// without them, so no flashing step has to re-ask whether they are
    /// there — [`confirm_identity`] refuses first, before anything is
    /// mounted for writing.
    pub fn payloads(&self) -> &'m Payloads {
        self.payloads
    }
}

/// Stage two of identify: match what the bootloader published against the
/// catalogue, then against what the bundle carries. Exact match, never a
/// substring — the T114's `Board-ID` is exactly `HT-n5262`, and a substring
/// rule is how a near-miss becomes a wrong write.
///
/// The two lookups fail with different errors on purpose (Codeberg #342):
/// "lnflash knows no board with that ID" and "this bundle carries no image
/// for it" have different remedies, and telling a user the wrong one sends
/// them after the wrong file.
pub fn confirm_identity<'m>(
    catalogue: &'m Catalogue,
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
    let Some((name, board)) = catalogue.board_for_id(board_id) else {
        return Err(Error::UnknownBoard {
            port: port.to_string(),
            board_id: board_id.to_string(),
            available: catalogue.names().iter().map(|s| s.to_string()).collect(),
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
    let payloads = manifest.payloads(name)?;
    Ok(Confirmed {
        name,
        board,
        payloads,
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
    /// What to do about the radio configuration once the board is up: ask,
    /// send what the flags already decided, or leave it alone.
    pub radio: RadioPlan,
    /// What to do about the telemetry target once the board is up (#236).
    /// The default is to ask, and the default answer to that is no.
    pub telemetry: TelemetryPlan,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            board: None,
            dry_run: false,
            appear_within: entry::BOOTLOADER_APPEARS_WITHIN,
            banner_window: verify::BANNER_WINDOW,
            radio: RadioPlan::default(),
            telemetry: TelemetryPlan::default(),
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

/// Everything on the bus lnflash has any business touching.
///
/// The catalogue and not the bundle, because the only thing read here is
/// which USB VID/PID pairs are LNodes — a hardware fact. That is what lets
/// the configure-only sessions run with no bundle on disk (Codeberg #342).
pub fn find_candidates(catalogue: &Catalogue, sysfs: &Sysfs) -> Result<Vec<Candidate>, Error> {
    let devices = sysfs.devices()?;
    let mut out: Vec<Candidate> = Vec::new();
    for (name, board) in &catalogue.board {
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
    /// What the radio step did, if it ran.
    pub radio: Option<RadioOutcome>,
    /// What the telemetry step did, if anything was sent at all.
    pub telemetry: Option<TelemetryOutcome>,
}

impl Outcome {
    /// Whether the firmware is on the board and confirmed.
    ///
    /// Neither the radio configuration nor the telemetry target is part of
    /// this. The flash has happened by the time those steps run, and a board
    /// that did not ACK is a board running our firmware on what it had
    /// stored — worth a warning, not worth reporting the flash as failed.
    pub fn is_good(&self) -> bool {
        self.application_written.is_some()
            && self.verdict.as_ref().is_some_and(Verdict::is_confirmed)
    }
}

/// What the radio step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioOutcome {
    pub settings: RadioSettings,
    /// Whether the board acknowledged the frame.
    pub acked: bool,
}

/// What the telemetry step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryOutcome {
    pub target: leviculum_core::envelope::TelemetryTargetWire,
    /// What the board answered to the target frame.
    pub reply: crate::envelope::SessionReply,
}

/// Resolve every candidate on the bus, individually.
pub fn run(
    catalogue: &Catalogue,
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
) -> Result<Vec<Outcome>, Error> {
    let candidates = find_candidates(catalogue, sysfs)?;
    if candidates.is_empty() {
        ui.say("No board lnflash knows is attached.");
        ui.say(&format!(
            "It knows: {}. Nothing to do.",
            catalogue.names().join(", ")
        ));
        // A board that is physically plugged in and still lands here is the
        // common case, not the exotic one: firmware that crashes before USB
        // comes up leaves nothing on the bus to find, and firmware without a
        // touch handler leaves nothing to knock on. Both are the same advice.
        // The hint goes here rather than on a per-device branch because
        // `Sysfs::devices` enumerates the whole bus — hubs included — so
        // "something is attached, but nothing we can act on" is true on every
        // host and carries no signal. A board we *do* see but cannot touch
        // already gets the double-tap prompt from `enter_bootloader`.
        ui.say(
            "If one is plugged in, what it is running neither enumerates nor answers the \
             1200-baud touch, so there is nothing here to knock on.",
        );
        ui.say("Double-tap RESET to hold it in its bootloader, then run this again.");
        return Ok(Vec::new());
    }

    ui.say(&format!("Found {} device(s):", candidates.len()));
    for candidate in &candidates {
        ui.say(&candidate.describe());
    }
    ui.say("");

    let mut outcomes = Vec::new();
    for candidate in candidates {
        match resolve(catalogue, manifest, sysfs, ui, opts, &candidate) {
            Ok(Some(outcome)) => outcomes.push(outcome),
            Ok(None) => {}
            // One board's refusal must not abandon the others: with several
            // devices attached each is its own decision.
            Err(err) => ui.say(&format!("{}: {err}\n", candidate.device.name)),
        }
    }
    Ok(outcomes)
}

/// One running board a configure session can talk to: where its transport
/// port is, and who the board is. The identity travels with the path so the
/// open can prove, after the fact, that the path still names this board —
/// a bare tty number resolved earlier can belong to a different board by
/// the time it is opened (#334 family).
struct ReachableBoard {
    port: String,
    tty: std::path::PathBuf,
    device: Device,
}

/// The boards that are already running, and the transport port on each.
///
/// The configure-without-flashing sessions all start here — "activation is
/// configuration, not firmware" means every one of them talks to a board
/// that is up, so finding them is written once.
struct Reachable {
    boards: Vec<ReachableBoard>,
    /// Boards found running whose transport port never appeared. Already
    /// reported to the user; counted so the session can still fail.
    unreachable: usize,
}

impl Reachable {
    /// True when nothing at all was found — the session has nothing to do.
    fn is_empty(&self) -> bool {
        self.boards.is_empty() && self.unreachable == 0
    }
}

fn reachable_boards(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
) -> Result<Reachable, Error> {
    let candidates = find_candidates(catalogue, sysfs)?;
    let mut found = Reachable {
        boards: Vec::new(),
        unreachable: 0,
    };
    for candidate in candidates.iter().filter(|c| !c.in_bootloader) {
        let port = candidate.device.name.clone();
        match entry::wait_for_interface_tty(
            sysfs,
            &candidate.device,
            radio::TRANSPORT_INTERFACE,
            Duration::from_secs(2),
        )? {
            Some(tty) => found.boards.push(ReachableBoard {
                port,
                tty,
                device: candidate.device.clone(),
            }),
            None => {
                ui.say(&format!(
                    "{port}: the transport port (if{:02}) never appeared, so nothing was sent.",
                    radio::TRANSPORT_INTERFACE
                ));
                found.unreachable += 1;
            }
        }
    }
    Ok(found)
}

/// Open a board's transport port and prove the binding.
///
/// The tty was resolved from a sysfs read that is history by the time this
/// open happens — on `--set-telemetry` with a prompt in between, arbitrarily
/// old — and `/dev/ttyACM` numbers are reused across re-enumerations, so
/// the path alone can name a different physical board by now. That is how a
/// session once wrote its frames into the void while the intended board's
/// witness saw zero bytes (#334 family, rig 2026-08-29).
///
/// The proof is taken after the open on purpose: an fd stays bound to the
/// driver instance it opened, so once the check passes, a later
/// re-enumeration kills the fd with EIO rather than retargeting it. A
/// by-id path narrows the resolve-to-open window; this closes it.
fn open_transport(sysfs: &Sysfs, device: &Device, tty: &Path) -> io::Result<crate::sys::Fd> {
    let fd = crate::sys::Fd::open_serial(tty)?;
    fd.set_transport_port()?;
    let Some(current) = sysfs
        .devices()?
        .into_iter()
        .find(|d| d.is_same_board(device))
    else {
        return Err(io::Error::other(format!(
            "{} is no longer on the bus; nothing was sent",
            device.name
        )));
    };
    let Some(tty_name) = current
        .interface(radio::TRANSPORT_INTERFACE)
        .and_then(|i| i.tty.clone())
    else {
        return Err(io::Error::other(format!(
            "{} has no transport port (if{:02}) any more; nothing was sent",
            current.name,
            radio::TRANSPORT_INTERFACE
        )));
    };
    let expected = sysfs.dev_path(&tty_name);
    let expected_rdev = std::os::unix::fs::MetadataExt::rdev(&std::fs::metadata(&expected)?);
    if fd.rdev()? != expected_rdev {
        return Err(io::Error::other(format!(
            "{} re-enumerated: its transport port is {} now, not {}; nothing was sent",
            current.name,
            expected.display(),
            tty.display()
        )));
    }
    Ok(fd)
}

/// The `--set-time` session (#238, #166 item 2): no flash, no bootloader
/// entry — find the boards already running, tell each what time it is
/// through the control envelope, report what each answered. `Ok(true)`
/// means every board that was found took the time.
///
/// Takes the catalogue and no bundle: nothing here reads a firmware image,
/// so requiring one on disk was an incidental dependency (Codeberg #342).
pub fn set_time(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    unix_secs: u64,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-time talks to flashed boards; a board in its \
             bootloader has no clock to set.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| send_time_to(&fd, unix_secs))
        {
            Ok(reply) => reply,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        all_took_it &= reply.took_it();
        match reply {
            SessionReply::Acked => ui.say(&format!(
                "{port}: time set — the board stamps from unix {unix_secs} now (source=host)."
            )),
            SessionReply::Refused(reason) => ui.say(&format!(
                "{port}: the board refused the time — {}.",
                crate::envelope::reason_str(reason)
            )),
            SessionReply::NoAnswer => ui.say(&format!(
                "{port}: the board did not answer the wall-time frame."
            )),
            SessionReply::NoEnvelope => ui.say(&format!(
                "{port}: this firmware predates the control envelope and cannot take a wall \
                 time. Flash the current bundle first."
            )),
            SessionReply::NotAccepted => ui.say(&format!(
                "{port}: this firmware speaks the envelope but does not accept the wall-time \
                 frame."
            )),
        }
    }
    Ok(all_took_it)
}

fn send_time_to(fd: &crate::sys::Fd, unix_secs: u64) -> io::Result<SessionReply> {
    use crate::envelope;
    envelope::probed(fd, leviculum_core::envelope::TYPE_WALL_TIME, |fd| {
        envelope::send_wall_time(fd, unix_secs)
    })
}

/// The `--set-tx-spacing` session (#345): no flash — find the boards
/// already running and tell each what gap to leave between the end of one
/// packet's airtime and the key-up of the next.
///
/// A bench instrument, and shaped like one: the value is not persisted on
/// the board, so a reset or a power cycle puts it back on the compiled
/// default. That is the point — a sweep must not be able to leave a board
/// silently spaced after the session that swept it.
pub fn set_tx_spacing(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    spacing_ms: u16,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-tx-spacing talks to flashed boards; a board in \
             its bootloader has no transmit path to space.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| send_tx_spacing_to(&fd, spacing_ms))
        {
            Ok(reply) => reply,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        all_took_it &= reply.took_it();
        match reply {
            SessionReply::Acked if spacing_ms == 0 => ui.say(&format!(
                "{port}: transmit spacing back to the compiled default — the board imposes no \
                 gap and transmits as it does out of the box."
            )),
            SessionReply::Acked => ui.say(&format!(
                "{port}: transmit spacing set to {spacing_ms} ms. The board logs \
                 [LORA_TX_SPACING] intended_ms=… waited_ms=… gap_ms=… on its debug port (if00) \
                 at every key-up; gap_ms is the gap that was actually on the air. Not \
                 persisted: a reset returns it to the default."
            )),
            SessionReply::Refused(reason) => ui.say(&format!(
                "{port}: the board refused the transmit spacing — {}.",
                crate::envelope::reason_str(reason)
            )),
            SessionReply::NoAnswer => ui.say(&format!(
                "{port}: the board did not answer the transmit-spacing frame, so it is still on \
                 whatever spacing it had."
            )),
            SessionReply::NoEnvelope => ui.say(&format!(
                "{port}: this firmware predates the control envelope and has no transmit-spacing \
                 knob. Flash the current bundle first."
            )),
            SessionReply::NotAccepted => ui.say(&format!(
                "{port}: this firmware speaks the envelope but has no transmit-spacing knob. \
                 Flash the current bundle first."
            )),
        }
    }
    Ok(all_took_it)
}

fn send_tx_spacing_to(fd: &crate::sys::Fd, spacing_ms: u16) -> io::Result<SessionReply> {
    use crate::envelope;
    envelope::probed(fd, leviculum_core::envelope::TYPE_TX_SPACING, |fd| {
        envelope::send_tx_spacing(fd, spacing_ms)
    })
}

/// The `--set-tx-power` session (Codeberg #349): set the transmit power on
/// every running LNode, no flashing, then exit.
///
/// Why it exists at all: the acceptance for #349 is a power sweep, and
/// without this each of its points costs a flash. A flash reboots the board,
/// which restarts the mesh, the duty-cycle histogram and every piece of state
/// the measurement is taken against — so six points would not be a sweep, they
/// would be six separate experiments with one number each.
///
/// **Read, modify, write.** A radio config frame carries the whole parameter
/// set, so the only honest way to move one field is to obtain the other ten
/// from the board first ([`crate::envelope::query_radio_config`], #349's
/// `TYPE_RADIO_QUERY`). Substituting this tool's own defaults for them would
/// reset the bandwidth while claiming to set the power, and the resulting
/// numbers would look like power. A board that cannot answer the query is
/// therefore left alone and said so — never written to on a guess.
///
/// **Persisted.** The board saves every radio config it applies, and this is
/// one, so a reset comes back on the swept power rather than on the previous
/// one. That is the opposite of `--set-tx-spacing`, which is deliberately
/// volatile; the difference is that spacing is a bench instrument and power
/// is part of the board's profile. Set it back when the sweep is over.
pub fn set_tx_power(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    dbm: i8,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-tx-power talks to flashed boards; a board in \
             its bootloader has no radio to configure.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| send_tx_power_to(&fd, dbm))
        {
            Ok(TxPowerOutcome::Applied { was, now }) => ui.say(&format!(
                "{port}: transmit power {was} -> {now} dBm requested. The other radio settings \
                 came back off the board and went out again unchanged. The board states what it \
                 programmed on its debug port (if00) as [SX_TX_POWER] requested_dbm=… \
                 tx_params_dbm=… clamped=… — read that line before measuring the point; a value \
                 outside the part's -9..=22 range is clamped there, not here. Persisted: a reset \
                 comes back on this power."
            )),
            Ok(TxPowerOutcome::Unreadable) => {
                all_took_it = false;
                ui.say(&format!(
                    "{port}: the board did not report its current radio settings, so nothing was \
                     sent. A config frame carries every parameter at once; writing one without \
                     knowing the other values would set the power and move the modulation with \
                     it. Flash the current bundle first."
                ));
            }
            Ok(TxPowerOutcome::Answered(reply)) => {
                all_took_it &= reply.took_it();
                match reply {
                    SessionReply::Acked => unreachable!("acked is reported as Applied"),
                    SessionReply::Refused(reason) => ui.say(&format!(
                        "{port}: the board refused the radio configuration — {}.",
                        crate::envelope::reason_str(reason)
                    )),
                    SessionReply::NoAnswer => ui.say(&format!(
                        "{port}: the board did not answer the radio-config frame, so it is still \
                         on whatever power it had."
                    )),
                    SessionReply::NoEnvelope => ui.say(&format!(
                        "{port}: this firmware predates the control envelope. Flash the current \
                         bundle first."
                    )),
                    SessionReply::NotAccepted => ui.say(&format!(
                        "{port}: this firmware speaks the envelope but takes no radio \
                         configuration. Flash the current bundle first."
                    )),
                }
            }
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
            }
        }
    }
    Ok(all_took_it)
}

/// What one board did with a `--set-tx-power` session.
enum TxPowerOutcome {
    /// The board reported its settings and acked the changed ones back.
    Applied { was: i8, now: i8 },
    /// The board never reported its settings, so nothing was sent.
    Unreadable,
    /// Settings were read and a config frame went out, but the board did not
    /// ack it.
    Answered(SessionReply),
}

fn send_tx_power_to(fd: &crate::sys::Fd, dbm: i8) -> io::Result<TxPowerOutcome> {
    use crate::envelope;
    use leviculum_core::envelope::{TYPE_RADIO_CONFIG, TYPE_RADIO_QUERY};

    // The probe first, and against BOTH types: the config frame is longer
    // than the 19-byte Reticulum minimum, so sending it on a guess is
    // packet-shaped noise on the transport CDC, and without the query there
    // is nothing to base it on anyway.
    let Some(caps) = envelope::probe_capabilities(fd)? else {
        return Ok(TxPowerOutcome::Answered(SessionReply::NoEnvelope));
    };
    if !caps.accepts(TYPE_RADIO_QUERY) || !caps.accepts(TYPE_RADIO_CONFIG) {
        return Ok(TxPowerOutcome::Answered(SessionReply::NotAccepted));
    }
    let Some(mut cfg) = envelope::query_radio_config(fd)? else {
        return Ok(TxPowerOutcome::Unreadable);
    };
    let was = cfg.tx_power_dbm;
    // One field. Everything else in `cfg` is the board's own answer, passed
    // straight back — this line is the whole read-modify-write contract.
    cfg.tx_power_dbm = dbm;
    match SessionReply::from(envelope::send_radio_config(fd, &cfg)?) {
        SessionReply::Acked => Ok(TxPowerOutcome::Applied { was, now: dbm }),
        other => Ok(TxPowerOutcome::Answered(other)),
    }
}

/// The `--set-telemetry` session (#236 scope item 5): the same telemetry
/// configuration the flash flow offers, without flashing anything.
/// Activation is configuration, so a board that is already running takes a
/// new target — or loses the one it had — over the same control envelope.
///
/// The question is asked once and the answer goes to every board found:
/// asking per board would make a two-board bench a two-address interview
/// for what is one decision.
pub fn set_telemetry(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    plan: &TelemetryPlan,
) -> Result<bool, Error> {
    // The target is decided before any port is resolved. The prompt can
    // hold this session open for as long as a human takes to find an LXMF
    // address, and a transport tty resolved before it can be renumbered by
    // the time it is opened — that stale number is the #334-family shape.
    // Resolving the plan touches no board, so nothing is lost by asking
    // first.
    let Some(target) = telemetry::resolve(ui, plan)? else {
        // Answering "no" to a session whose whole purpose was to configure
        // is a clean exit, not a failure: nothing was asked for and nothing
        // was changed.
        ui.say("Telemetry left as it is; nothing was sent.");
        return Ok(true);
    };

    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-telemetry talks to flashed boards; a board in \
             its bootloader has no telemetry to configure.",
        );
        return Ok(false);
    }

    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| telemetry::send_configured(&fd, &target))
        {
            Ok(reply) => reply,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        all_took_it &= reply.took_it();
        report_telemetry(ui, port, &target, reply);
    }
    Ok(all_took_it)
}

/// Say what the board answered to the telemetry target, in the same words
/// on the flash path and on the standalone path.
///
/// The node's own `[TELEMETRY] target=… state=…` line is not read back
/// here. It goes to the debug CDC (if00), and this tool holds that port
/// open only for the boot check in [`verify_boot`], which is finished and
/// closed by the time telemetry is configured — and on `--set-telemetry`
/// it never opens at all. Opening it a second time to read our own effect
/// back would be a second connection to a board mid-configuration, so the
/// ack is what is reported and the operator is told where the node says the
/// rest.
fn report_telemetry(
    ui: &mut dyn Ui,
    port: &str,
    target: &leviculum_core::envelope::TelemetryTargetWire,
    reply: SessionReply,
) {
    use leviculum_core::envelope::TELEMETRY_PROFILE_OFF;
    match reply {
        SessionReply::Acked if target.profile == TELEMETRY_PROFILE_OFF => ui.say(&format!(
            "{port}: telemetry off — the board acknowledged the cleared target and stops \
             reporting."
        )),
        SessionReply::Acked => {
            ui.say(&format!(
                "{port}: telemetry on — {}.",
                telemetry::describe(target)
            ));
            if target.public_key.is_none() {
                ui.say(&format!(
                    "{port}: the node has no key for that address yet, so it asks the mesh for \
                     one; it reports [TELEMETRY] target=… state=awaiting-key on its debug port \
                     (if00) until an announce answers, then state=ready."
                ));
            } else {
                ui.say(&format!(
                    "{port}: the key travelled with the target, so the node reports \
                     [TELEMETRY] target=… state=ready on its debug port (if00)."
                ));
            }
        }
        SessionReply::Refused(reason) => ui.say(&format!(
            "{port}: the board refused the telemetry target — {}.",
            crate::envelope::reason_str(reason)
        )),
        SessionReply::NoAnswer => ui.say(&format!(
            "{port}: the board did not answer the telemetry frame, so it is still on whatever \
             target it had stored."
        )),
        SessionReply::NoEnvelope => ui.say(&format!(
            "{port}: this firmware predates the control envelope and cannot take a telemetry \
             target. Flash the current bundle first."
        )),
        SessionReply::NotAccepted => ui.say(&format!(
            "{port}: this firmware speaks the envelope but has no telemetry consumer. Flash \
             the current bundle first."
        )),
    }
}

fn resolve(
    catalogue: &Catalogue,
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
        enter_bootloader(catalogue, sysfs, ui, opts, candidate)?
    };
    let Some(block) = bootloader.block_device() else {
        return Err(transport::Error::NoDrive { port }.into());
    };

    let drive = Drive::open(&block)?;
    let info = drive.info()?;
    let confirmed = confirm_identity(catalogue, manifest, &info, &port, opts.board.as_deref())?;
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
    let app_image = prepare(&confirmed.payloads().app, &manifest.root, &confirmed)?;

    // Everything that could refuse has refused by now, so the plan the user
    // is shown is the plan that will run.
    let mut plan: Vec<String> = Vec::new();
    let remedy_image = match &precondition {
        Precondition::Met => None,
        needs => {
            let Some(remedy) = &confirmed.payloads().remedy.softdevice else {
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
        confirmed.payloads().app.file.display(),
        confirmed
            .payloads()
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
        radio: None,
        telemetry: None,
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
        let again = back_to_bootloader(catalogue, sysfs, ui, opts, &bootloader, confirmed.name())?;
        let block = again
            .block_device()
            .ok_or_else(|| transport::Error::NoDrive { port: port.clone() })?;
        drive = Drive::open(&block)?;
        // Identity is confirmed again rather than carried over: this is a
        // fresh mount of a drive that reappeared, and the rule is the rule.
        let info = drive.info()?;
        confirm_identity(catalogue, manifest, &info, &port, Some(confirmed.name()))?;
        let after = read_installed(&drive, &info);
        ui.say(&format!("{port}: SoftDevice now {}", after.describe()));
    }

    let declined = app_image.blocks_below(confirmed.board().flash.writable_start);
    let written = drive.write_image("APP.UF2", &app_image, declined)?;
    report_write(ui, &port, "application", &written);
    drive.close()?;
    outcome.application_written = Some(written);

    let booted = verify_boot(sysfs, ui, opts, &bootloader, &confirmed, &port)?;
    // The radio step needs a port to talk to, so it runs whenever the
    // application came back — including on an unconfirmed banner, where the
    // board is up and the only thing missing is the proof of which build.
    if let Some(app) = &booted.app {
        outcome.radio = set_radio(sysfs, ui, opts, app, &port)?;
        outcome.telemetry = set_telemetry_on(sysfs, ui, opts, app, &port)?;
    }
    outcome.verdict = Some(booted.verdict);
    Ok(Some(outcome))
}

/// Ask whether this board should send telemetry, and if so to where (#236).
///
/// Runs after the radio step because it is the same shape of question about
/// the same board on the same port, and because a user answering "no" to
/// the one question this adds should meet it once the board is otherwise
/// finished. Never fails the run, for the reason [`set_radio`] gives: the
/// firmware is already written and confirmed.
fn set_telemetry_on(
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    app: &Device,
    port: &str,
) -> Result<Option<TelemetryOutcome>, Error> {
    let Some(target) = telemetry::resolve(ui, &opts.telemetry)? else {
        return Ok(None);
    };

    let Some(tty) =
        entry::wait_for_interface_tty(sysfs, app, radio::TRANSPORT_INTERFACE, opts.appear_within)?
    else {
        ui.say(&format!(
            "{port}: the firmware is on the board, but its transport port (if{:02}) never \
             appeared, so the telemetry target was not sent. Re-run `lnflash --set-telemetry` \
             once it enumerates.",
            radio::TRANSPORT_INTERFACE
        ));
        return Ok(Some(TelemetryOutcome {
            target,
            reply: SessionReply::NoAnswer,
        }));
    };

    ui.say(&format!(
        "{port}: sending the telemetry target to {} — {}",
        tty.display(),
        telemetry::describe(&target)
    ));
    let reply = match open_transport(sysfs, app, &tty)
        .and_then(|fd| telemetry::send_configured(&fd, &target))
    {
        Ok(reply) => reply,
        Err(err) => {
            ui.say(&format!(
                "{port}: the transport port could not be used ({err})"
            ));
            SessionReply::NoAnswer
        }
    };
    report_telemetry(ui, port, &target, reply);
    Ok(Some(TelemetryOutcome { target, reply }))
}

/// Choose a radio configuration, send it, and say what happened.
///
/// Never fails the run: by the time this is reached the firmware is written
/// and confirmed, and a board that does not take the configuration is a
/// board running the compiled default. Only a prompt that cannot be read at
/// all propagates, and that is the user's terminal going away.
fn set_radio(
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    app: &Device,
    port: &str,
) -> Result<Option<RadioOutcome>, Error> {
    let Some(choice) = resolve_radio_choice(ui, opts)? else {
        return Ok(None);
    };
    let settings = choice.settings();

    let Some(tty) =
        entry::wait_for_interface_tty(sysfs, app, radio::TRANSPORT_INTERFACE, opts.appear_within)?
    else {
        ui.say(&format!(
            "{port}: the firmware is on the board, but its transport port (if{:02}) never \
             appeared, so the radio settings were not sent. The board is running whatever it \
             had stored. Re-run lnflash once it enumerates to set them.",
            radio::TRANSPORT_INTERFACE
        ));
        return Ok(Some(RadioOutcome {
            settings,
            acked: false,
        }));
    };

    ui.say(&format!(
        "{port}: sending radio settings to {} — {}",
        tty.display(),
        settings.describe()
    ));
    let acked = match open_transport(sysfs, app, &tty)
        .and_then(|fd| radio::send_configured(&fd, &settings))
    {
        Ok(acked) => acked,
        Err(err) => {
            ui.say(&format!(
                "{port}: the transport port could not be used ({err})"
            ));
            false
        }
    };
    if acked {
        ui.say(&format!(
            "{port}: radio settings written and persisted: {}",
            settings.describe()
        ));
    } else {
        ui.say(&format!(
            "{port}: the firmware flashed fine, but the board did not acknowledge the radio \
             settings, so it is still on whatever it had stored — the compiled default on a \
             board that never had any. Nothing is broken: re-run lnflash, or set them from the \
             host that binds the board."
        ));
    }
    Ok(Some(RadioOutcome { settings, acked }))
}

/// Turn the plan into a choice, and say what the chosen preset obliges the
/// user to read (the us915 FCC note) — here, so the flag path and the menu
/// path cannot diverge on whether the note is printed. `None` is `Skip`.
fn resolve_radio_choice(ui: &mut dyn Ui, opts: &Options) -> Result<Option<RadioChoice>, Error> {
    let choice = match &opts.radio {
        RadioPlan::Skip => return Ok(None),
        RadioPlan::Fixed(choice) => choice.clone(),
        RadioPlan::Ask => ask_for_radio(ui)?,
    };
    if let RadioChoice::Preset(preset) = &choice {
        if let Some(caveat) = preset.caveat {
            ui.say(caveat);
        }
    }
    Ok(Some(choice))
}

/// The eu868 preset, which is what "the default radio settings" means.
fn default_preset() -> &'static radio::PresetDef {
    radio::preset("eu868").expect("the shipped table carries eu868")
}

/// The prompts. The default answer to every one of them is the eu868 value,
/// so a user who holds Enter down gets a board configured explicitly rather
/// than a half-finished one.
fn ask_for_radio(ui: &mut dyn Ui) -> Result<RadioChoice, Error> {
    let eu868 = default_preset();
    let defaults = eu868.settings;
    let answer = ui.ask(&format!(
        "\nFlash default radio settings (eu868, ReticulumNet consensus: {:.3} MHz, SF{}, BW{}, \
         CR4/{}, {} dBm)? [Y/n]",
        defaults.frequency_hz as f64 / 1e6,
        defaults.sf,
        defaults.bandwidth_hz / 1000,
        defaults.cr,
        defaults.tx_power_dbm
    ))?;
    let wants_default = !matches!(
        answer
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "n" | "no"
    );
    if wants_default {
        return Ok(RadioChoice::Preset(eu868));
    }

    // "Not the defaults" opens the preset menu; the last entry is the
    // field-by-field path. The menu is built from the table, so an entry
    // added there appears here without a second list to update.
    let presets: Vec<_> = radio::selectable().collect();
    loop {
        for (i, preset) in presets.iter().enumerate() {
            ui.say(&format!("  {}) {}", i + 1, preset.menu_label));
        }
        ui.say(&format!("  {}) custom", presets.len() + 1));
        let Some(answer) = ui.ask("  preset [1]")? else {
            // Enter, or no terminal to ask: the pre-selected eu868 stands.
            return Ok(RadioChoice::Preset(eu868));
        };
        match answer.trim().parse::<usize>() {
            Ok(n) if (1..=presets.len()).contains(&n) => {
                return Ok(RadioChoice::Preset(presets[n - 1]));
            }
            Ok(n) if n == presets.len() + 1 => break,
            _ => ui.say(&format!(
                "  {:?} is not one of the options; 1-{}",
                answer,
                presets.len() + 1
            )),
        }
    }

    let mut settings = defaults;
    for field in radio::FIELDS {
        loop {
            let prompt = format!("  {} [{}]", field.label(), field.value_of(&settings));
            let Some(answer) = ui.ask(&prompt)? else {
                // Enter, or no terminal to ask: the shown default stands.
                break;
            };
            match field.apply(&mut settings, &answer) {
                Ok(()) => break,
                Err(err) => ui.say(&format!("  {err}")),
            }
        }
    }
    Ok(RadioChoice::Custom(settings))
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
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    candidate: &Candidate,
) -> Result<Device, Error> {
    // The hint is enough to choose *how to knock*; it is not enough to
    // choose what to write, which is why identity is confirmed afterwards.
    let board = catalogue.board(&candidate.hint)?;
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
                ui.wait_for_human(&entry::double_tap_instruction(
                    &format!("The board on {port}"),
                    &board.double_tap,
                ))?;
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
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    was: &Device,
    board_name: &str,
) -> Result<Device, Error> {
    let board = catalogue.board(board_name)?;
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
    enter_bootloader(catalogue, sysfs, ui, opts, &candidate)
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

/// What the verify step saw: the judgement, and the device it judged.
///
/// The device is carried out rather than dropped because the radio step
/// needs a port on the board that just came back, and re-deriving "which
/// device is that" from the bus a second time is how the wrong board gets
/// written to.
struct Booted {
    verdict: Verdict,
    app: Option<Device>,
}

fn verify_boot(
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    was: &Device,
    confirmed: &Confirmed,
    port: &str,
) -> Result<Booted, Error> {
    let board = confirmed.board();
    let ids = board.candidate_ids(confirmed.name())?;
    // The bootloader drive going away is the first half of "it took"; the
    // application coming back is the second. Waiting for the first also
    // keeps a stale sysfs entry from answering the second.
    entry::wait_until_gone(sysfs, was, opts.appear_within)?;
    let Some(app) = entry::wait_for_application(sysfs, &ids, Some(was), opts.appear_within)? else {
        ui.say(&format!("{port}: the application never came back."));
        return Ok(Booted {
            verdict: Verdict::Absent,
            app: None,
        });
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
    let verdict = verify::judge(banner, confirmed.payloads().app.git_sha.as_deref());
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
    Ok(Booted {
        verdict,
        app: Some(app),
    })
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

    fn catalogue() -> Catalogue {
        Catalogue::builtin().unwrap()
    }

    /// A one-board bundle, with the real vendored SoftDevice hex so the
    /// remedy path is prepared from the real image. The board facts it is
    /// checked against are the compiled-in catalogue's, so these tests run
    /// against the same t114 entry the shipped binary uses.
    fn bundle() -> (TempDir, Catalogue, Manifest) {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("t114")).unwrap();
        let hex = include_str!("../payload/t114/s140_nrf52_7.3.0_softdevice.hex");
        fs::write(dir.path().join("t114/sd.hex"), hex).unwrap();
        fs::write(
            dir.path()
                .join("t114/s140_nrf52_7.3.0_license-agreement.txt"),
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

[board.t114.app]
file    = "t114/app.uf2"
sha256  = "{app_sha}"
git_sha = "bb7c4f64"

[board.t114.remedy.softdevice]
file    = "t114/sd.hex"
sha256  = "{sd_sha}"
license = "t114/s140_nrf52_7.3.0_license-agreement.txt"
convert = "hex-to-uf2"
"#,
            app_sha = manifest::hex_digest(&app),
            sd_sha = manifest::hex_digest(hex.as_bytes()),
        );
        fs::write(dir.path().join("manifest.toml"), text).unwrap();
        let manifest = load(dir.path(), &catalogue()).unwrap();
        (dir, catalogue(), manifest)
    }

    /// The bundle `scripts/lnflash-bundle.sh` builds since Codeberg #261: an
    /// image per board, a SoftDevice remedy only where one is vendored.
    ///
    /// The RAK image is a distinct byte pattern on purpose — "which image did
    /// this board get" is the question 362c1c2d got wrong, and two identical
    /// payloads could not answer it.
    fn two_board_bundle() -> (TempDir, Catalogue, Manifest) {
        let (dir, catalogue, _) = bundle();
        fs::create_dir_all(dir.path().join("rak4631")).unwrap();
        let app = Image::from_spans(
            &[crate::ihex::Span {
                start: 0x2_7000,
                data: vec![0xCD; 0x1000],
            }],
            crate::uf2::FAMILY_NRF52840_APP,
        )
        .encode()
        .unwrap();
        fs::write(dir.path().join("rak4631/app.uf2"), &app).unwrap();

        let path = dir.path().join("manifest.toml");
        let text = format!(
            "{}\n[board.rak4631.app]\nfile    = \"rak4631/app.uf2\"\nsha256  = \"{}\"\n\
             git_sha = \"bb7c4f64\"\n",
            fs::read_to_string(&path).unwrap(),
            manifest::hex_digest(&app),
        );
        fs::write(&path, text).unwrap();
        let manifest = load(dir.path(), &catalogue).unwrap();
        (dir, catalogue, manifest)
    }

    fn sysfs() -> Sysfs {
        Sysfs::new(crate::sysfs_fixture::materialized())
    }

    #[test]
    fn the_board_the_bootloader_names_is_the_board_that_gets_confirmed() {
        let (_dir, catalogue, manifest) = bundle();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
        )
        .unwrap();
        assert_eq!(confirmed.name(), "t114");
        assert_eq!(confirmed.board().flash.app_base, 0x2_7000);
    }

    #[test]
    fn a_rak_in_the_bootloader_is_refused_by_a_t114_only_bundle() {
        // The 362c1c2d failure, in one assertion: a T114 image must not land
        // on a RAK because the drive looked the same.
        //
        // Since #261 the catalogue knows the RAK, so the refusal comes from
        // the bundle rather than from the catalogue — NoImage, not
        // UnknownBoard. That is the #342 distinction doing its job and it has
        // to survive: a bundle built before #261 is still a valid bundle, and
        // its user needs to be sent after a newer tarball, not a newer binary.
        let (_dir, catalogue, manifest) = bundle();
        let err = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(RAK_INFO),
            "3-2.4",
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Manifest(manifest::Error::NoImage { .. })),
            "{err:?}"
        );
        assert!(format!("{err}").contains("rak4631"), "{err}");
        assert!(format!("{err}").contains("carries t114"), "{err}");
    }

    #[test]
    fn a_rak_in_the_bootloader_is_flashed_from_a_bundle_that_carries_one() {
        // The other half of #261: the same INFO_UF2.TXT that is refused above
        // confirms a RAK once the bundle has an image for it, and it confirms
        // the RAK's own facts rather than the T114's.
        let (_dir, catalogue, manifest) = two_board_bundle();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(RAK_INFO),
            "3-2.3.4.4",
            None,
        )
        .unwrap();
        assert_eq!(confirmed.name(), "rak4631");
        assert_eq!(confirmed.board().flash.app_base, 0x2_7000);
        assert_eq!(
            confirmed.board().identify.msc_label.as_deref(),
            Some("RAK4631")
        );
        // ...and the T114 on the same bench still resolves to the T114 image.
        let t114 = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
        )
        .unwrap();
        assert_eq!(t114.name(), "t114");
        assert_ne!(
            t114.payloads().app.file,
            confirmed.payloads().app.file,
            "two boards on one bus must not be handed the same image"
        );
        // Each image is prepared against its own board's window.
        prepare(&confirmed.payloads().app, &manifest.root, &confirmed).unwrap();
        prepare(&t114.payloads().app, &manifest.root, &t114).unwrap();
    }

    #[test]
    fn asking_for_the_rak_and_finding_a_t114_refuses_even_when_both_are_carried() {
        let (_dir, catalogue, manifest) = two_board_bundle();
        let err = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            Some("rak4631"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::WrongBoard { .. }), "{err}");
        assert!(format!("{err}").contains("Nothing was written"), "{err}");
    }

    #[test]
    fn asking_for_one_board_and_finding_another_refuses_rather_than_flashing_it() {
        let (_dir, catalogue, manifest) = bundle();
        // `--board rak4631` with a T114 on the bench: the bootloader says
        // T114, so the run stops rather than writing what was asked for.
        let err = confirm_identity(
            &catalogue,
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
            confirm_identity(
                &catalogue,
                &manifest,
                &infouf2::parse(T114_INFO),
                "3-2.4",
                Some("t114")
            )
            .unwrap()
            .name(),
            "t114"
        );
    }

    #[test]
    fn a_bootloader_that_publishes_no_board_id_gets_nothing_written_to_it() {
        let (_dir, catalogue, manifest) = bundle();
        let info = infouf2::parse("UF2 Bootloader 0.9.0\r\nDate: Jul  9 2024\r\n");
        assert!(matches!(
            confirm_identity(&catalogue, &manifest, &info, "3-2.4", None),
            Err(Error::NoBoardId { .. })
        ));
        // An empty value is the same as no value: it confirms nothing.
        let blank = infouf2::parse("Board-ID:  \r\n");
        assert!(matches!(
            confirm_identity(&catalogue, &manifest, &blank, "3-2.4", None),
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
        let (_dir, catalogue, manifest) = bundle();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
        )
        .unwrap();
        let remedy = &confirmed
            .payloads()
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
        let (dir, catalogue, manifest) = bundle();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
        )
        .unwrap();
        fs::write(dir.path().join("t114/app.uf2"), b"tampered").unwrap();
        let err = prepare(&confirmed.payloads().app, &manifest.root, &confirmed).unwrap_err();
        assert!(matches!(
            err,
            Error::Manifest(manifest::Error::Checksum { .. })
        ));
    }

    #[test]
    fn an_image_reaching_past_the_writable_window_is_refused_before_any_mount() {
        let (dir, catalogue, manifest) = bundle();
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
                &manifest.payloads("t114").unwrap().app.sha256,
                &manifest::hex_digest(&too_high),
            );
        fs::write(dir.path().join("manifest.toml"), text).unwrap();
        let manifest = load(dir.path(), &catalogue).unwrap();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
        )
        .unwrap();
        let err = prepare(&confirmed.payloads().app, &manifest.root, &confirmed).unwrap_err();
        assert!(matches!(err, Error::OutsideWindow { .. }), "{err}");
        assert!(format!("{err}").contains("Nothing was written"));
    }

    #[test]
    fn candidates_are_found_by_usb_id_and_labelled_as_hints_only() {
        // No bundle in this test at all, which is the #342 property itself:
        // finding what is on the bus reads USB IDs and nothing else, so the
        // catalogue alone is enough.
        let found = find_candidates(&catalogue(), &sysfs()).unwrap();
        let names: Vec<&str> = found.iter().map(|c| c.device.name.as_str()).collect();
        // 3-2.3.1 is our T114 application, 3-2.4 its bootloader, and 3-2.3.4.4
        // our RAK4631 application on 1209:0002. Before #261 the last one was
        // invisible: the catalogue listed no board claiming that ID, so
        // `lnflash --set-time` addressed the two T114s and silently skipped
        // the Pocket V2 sitting on the same hub.
        assert_eq!(names, vec!["3-2.3.1", "3-2.3.4.4", "3-2.4"]);
        assert!(!found[0].in_bootloader);
        assert!(!found[1].in_bootloader);
        assert!(found[2].in_bootloader);
        // And each is hinted at its own board rather than at whichever entry
        // the catalogue happens to list first.
        assert!(
            found[0].describe().contains("probably a t114"),
            "{:?}",
            found[0].describe()
        );
        assert!(
            found[1].describe().contains("probably a rak4631"),
            "{:?}",
            found[1].describe()
        );
        assert!(found[2].describe().contains("probably a t114"));
    }

    #[test]
    fn the_configure_only_sessions_address_the_rak_as_well_as_the_t114() {
        // Codeberg #261, seen from the rig: `--set-time` walked the bus, found
        // both T114s and never spoke to the Pocket V2, because no catalogue
        // entry claimed its USB ID.
        //
        // This stops at the enumeration step on purpose. Everything past it
        // opens the board's transport port, and this suite runs on the host
        // that has the rig attached — a test that got as far as writing a
        // wall-time frame would be writing it to somebody's real board. The
        // bus is the stub; the dev tree is a stub too, so the host's real
        // /dev/serial/by-id (which on the rig names real boards) cannot leak
        // into the resolved paths.
        let dev = TempDir::new().unwrap();
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());
        let mut ui = crate::ui::testing::Fake::agreeing();
        let found = reachable_boards(&catalogue(), &sysfs, &mut ui).unwrap();
        assert_eq!(found.unreachable, 0, "{}", ui.transcript());
        let ports: Vec<(&str, PathBuf)> = found
            .boards
            .iter()
            .map(|b| (b.port.as_str(), b.tty.clone()))
            .collect();
        assert_eq!(
            ports,
            vec![
                ("3-2.3.1", dev.path().join("ttyACM2")),
                ("3-2.3.4.4", dev.path().join("ttyACM4")),
            ],
            "both running boards, each on its own transport port (if02)"
        );
        // The bootloader on 3-2.4 has no clock to set and is not addressed.
        assert!(!ports.iter().any(|(port, _)| *port == "3-2.4"));
        // And each entry carries the identity of the board it was resolved
        // for, which is what the open will later be checked against.
        assert_eq!(
            found.boards[0].device.serial.as_deref(),
            Some("183004F712B4A7FE")
        );
    }

    // -----------------------------------------------------------------
    // Port targeting, and the proof taken at open (#334 family)
    // -----------------------------------------------------------------

    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    /// A dev tree whose ttys are pseudo-terminals: opening one is safe on
    /// any host, including the one with the rig attached.
    fn dev_tree(ttys: &[(&str, &Path)]) -> TempDir {
        let dev = TempDir::new().unwrap();
        for (name, target) in ttys {
            symlink(target, dev.path().join(name)).unwrap();
        }
        dev
    }

    #[test]
    fn with_udev_present_the_boards_are_addressed_by_id_rather_than_by_number() {
        // The project rule, asserted at the seam that violated it: a
        // ttyACM number is an allocation slot the kernel reuses, a by-id
        // link is the board's own identity, re-pointed by udev across a
        // re-enumeration. When the link exists, it is the path a session
        // gets — multi-board included, each board its own link.
        let dev = TempDir::new().unwrap();
        let by_id = dev.path().join("serial/by-id");
        fs::create_dir_all(&by_id).unwrap();
        symlink("../../ttyACM2", by_id.join("usb-leviculum_T114-if02")).unwrap();
        symlink("../../ttyACM4", by_id.join("usb-leviculum_RAK4631-if02")).unwrap();

        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());
        let mut ui = crate::ui::testing::Fake::agreeing();
        let found = reachable_boards(&catalogue(), &sysfs, &mut ui).unwrap();
        let ports: Vec<PathBuf> = found.boards.iter().map(|b| b.tty.clone()).collect();
        assert_eq!(
            ports,
            vec![
                by_id.join("usb-leviculum_T114-if02"),
                by_id.join("usb-leviculum_RAK4631-if02"),
            ]
        );
    }

    #[test]
    fn the_open_proves_it_landed_on_the_board_the_path_was_resolved_for() {
        // The happy path of the proof: the tty still belongs to the board,
        // so the fd's device number matches the node a fresh bus read names.
        let pty = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[("ttyACM2", &pty.slave_path)]);
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());
        let t114 = sysfs
            .devices()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "3-2.3.1")
            .unwrap();
        open_transport(&sysfs, &t114, &dev.path().join("ttyACM2")).unwrap();
    }

    #[test]
    fn a_renumbered_port_is_refused_at_open_rather_than_written_into() {
        // The 2026-08-29 rig failure, host-side: the board re-enumerated
        // after its tty was resolved, the number was reused, and the frames
        // went into whatever held it — while the session reported success.
        // Now the open compares the fd against where the board actually is
        // and refuses.
        let tree = TempDir::new().unwrap();
        crate::sysfs_fixture::materialized_copy(tree.path());
        let sysfs_before = Sysfs::new(tree.path());
        let t114 = sysfs_before
            .devices()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "3-2.3.1")
            .unwrap();

        // The board re-enumerates: its transport tty is ttyACM7 now, and
        // the freed ttyACM2 belongs to something else (a second pty here).
        let iface_tty = tree.path().join("3-2.3.1:1.2/tty");
        fs::rename(iface_tty.join("ttyACM2"), iface_tty.join("ttyACM7")).unwrap();
        let stale = crate::sys::testpty::Pty::open();
        let current = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[
            ("ttyACM2", &stale.slave_path),
            ("ttyACM7", &current.slave_path),
        ]);
        let sysfs = Sysfs::with_dev(tree.path(), dev.path());

        let err = open_transport(&sysfs, &t114, &dev.path().join("ttyACM2")).unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("re-enumerated"), "{text}");
        assert!(text.contains("nothing was sent"), "{text}");
        // And the path it names as current is the one that would work.
        assert!(text.contains("ttyACM7"), "{text}");

        // The board's actual port still opens.
        open_transport(&sysfs, &t114, &dev.path().join("ttyACM7")).unwrap();
    }

    #[test]
    fn a_board_that_left_the_bus_is_refused_at_open_rather_than_guessed_at() {
        let tree = TempDir::new().unwrap();
        crate::sysfs_fixture::materialized_copy(tree.path());
        let sysfs_before = Sysfs::new(tree.path());
        let t114 = sysfs_before
            .devices()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "3-2.3.1")
            .unwrap();

        let remove_prefix = |prefix: &str| {
            for entry in fs::read_dir(tree.path()).unwrap().flatten() {
                let name = entry.file_name().into_string().unwrap();
                if name.starts_with(prefix) {
                    fs::remove_dir_all(entry.path()).unwrap();
                }
            }
        };
        let pty = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[("ttyACM2", &pty.slave_path)]);

        // First the application entry goes: the fixture's 3-2.4 bootloader
        // is the same physical T114 (word-swapped serial), so the board is
        // still found — in a mode with no transport port, which is its own
        // refusal, not a guess.
        remove_prefix("3-2.3.1");
        let sysfs = Sysfs::with_dev(tree.path(), dev.path());
        let err = open_transport(&sysfs, &t114, &dev.path().join("ttyACM2")).unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("no transport port"), "{text}");
        assert!(text.contains("nothing was sent"), "{text}");

        // Then the bootloader too: the board is gone entirely.
        remove_prefix("3-2.4");
        let err = open_transport(&sysfs, &t114, &dev.path().join("ttyACM2")).unwrap_err();
        let text = format!("{err}");
        assert!(text.contains("no longer on the bus"), "{text}");
        assert!(text.contains("nothing was sent"), "{text}");
    }

    #[test]
    fn a_session_whose_frames_land_in_a_void_fails_instead_of_reporting_success() {
        // The false-success half of the 2026-08-29 finding, pinned: both
        // transport ports open fine but swallow every byte (a pty with
        // nothing scripted on the master side — bytes go in, nothing comes
        // back). --set-telemetry persists a flash setting; "wrote bytes to
        // an fd" must not exit as success, only the board's ack may.
        let t114_pty = crate::sys::testpty::Pty::open();
        let rak_pty = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[
            ("ttyACM2", &t114_pty.slave_path),
            ("ttyACM4", &rak_pty.slave_path),
        ]);
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());

        let mut ui = crate::ui::testing::Fake::agreeing();
        let plan = TelemetryPlan::Fixed(station_target());
        let ok = set_telemetry(&catalogue(), &sysfs, &mut ui, &plan).unwrap();
        assert!(!ok, "silence must fail the session:\n{}", ui.transcript());
        assert!(
            !ui.transcript().contains("telemetry on"),
            "{}",
            ui.transcript()
        );
    }

    #[test]
    fn a_board_that_acks_the_target_is_a_successful_session() {
        // The control for the test above: the same wiring with firmware on
        // the other end, and the session succeeds on the ack. Uses the
        // scripted stub that runs the firmware's real decision function.
        let t114_pty = crate::sys::testpty::Pty::open();
        let rak_pty = crate::sys::testpty::Pty::open();
        crate::envelope::testing::envelope_firmware_stub(
            &t114_pty,
            crate::envelope::testing::seen(),
        );
        crate::envelope::testing::envelope_firmware_stub(
            &rak_pty,
            crate::envelope::testing::seen(),
        );
        let dev = dev_tree(&[
            ("ttyACM2", &t114_pty.slave_path),
            ("ttyACM4", &rak_pty.slave_path),
        ]);
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());

        let mut ui = crate::ui::testing::Fake::agreeing();
        let plan = TelemetryPlan::Fixed(station_target());
        let ok = set_telemetry(&catalogue(), &sysfs, &mut ui, &plan).unwrap();
        assert!(ok, "{}", ui.transcript());
        assert!(
            ui.transcript().contains("telemetry on"),
            "{}",
            ui.transcript()
        );
    }

    #[test]
    fn the_target_is_decided_before_any_port_is_resolved() {
        // The prompt can hold --set-telemetry open for as long as a human
        // takes, and a tty resolved before it may be renumbered by the time
        // it is used (#334 family). Answering "no" therefore has to end the
        // session before the bus is even read: an unreadable sysfs proves
        // nothing was resolved.
        let mut ui = crate::ui::testing::Fake::agreeing();
        let ok = set_telemetry(
            &catalogue(),
            &Sysfs::new("/nonexistent/sysfs/root"),
            &mut ui,
            &TelemetryPlan::default(),
        )
        .unwrap();
        assert!(ok, "answering no is a clean exit");
        assert!(
            ui.transcript().contains("Telemetry left as it is"),
            "{}",
            ui.transcript()
        );
    }

    #[test]
    fn an_empty_bus_is_reported_rather_than_waited_on() {
        let (_dir, catalogue, manifest) = bundle();
        let empty = TempDir::new().unwrap();
        let mut ui = crate::ui::testing::Fake::agreeing();
        let outcomes = run(
            &catalogue,
            &manifest,
            &Sysfs::new(empty.path()),
            &mut ui,
            &Options::default(),
        )
        .unwrap();
        assert!(outcomes.is_empty());
        let said = ui.transcript();
        assert!(said.contains("No board lnflash knows"));
        assert!(said.contains("t114"));
        // A dark board — crashed firmware, or firmware linked for a base this
        // bootloader does not run — is invisible on USB and lands exactly
        // here, so the only way out has to be said.
        assert!(said.contains("Double-tap RESET"), "{said}");
        assert!(said.contains("1200-baud touch"), "{said}");
    }

    #[test]
    fn pressing_enter_at_the_radio_prompt_flashes_the_eu_defaults() {
        let mut ui = crate::ui::testing::Fake::agreeing();
        assert_eq!(
            ask_for_radio(&mut ui).unwrap(),
            RadioChoice::Preset(default_preset())
        );
        let said = ui.transcript();
        assert!(said.contains("[Y/n]"), "{said}");
        assert!(said.contains("ReticulumNet consensus"), "{said}");
        assert!(said.contains("869.463 MHz"), "{said}");
        assert!(said.contains("SF8") && said.contains("BW125"), "{said}");
        assert!(said.contains("CR4/5") && said.contains("22 dBm"), "{said}");
        // Saying yes must not then walk the user through menu or prompts.
        assert!(!said.contains("spreadingfactor"), "{said}");
        assert!(!said.contains("us915"), "{said}");
    }

    #[test]
    fn yes_mode_takes_the_defaults_rather_than_waiting_on_the_prompt() {
        // The automation case: --yes must not block on a question nobody is
        // there to answer.
        let mut ui = crate::ui::Assumed::new(true);
        assert_eq!(
            ask_for_radio(&mut ui).unwrap(),
            RadioChoice::Preset(default_preset())
        );
    }

    #[test]
    fn declining_the_defaults_opens_the_menu_and_two_picks_us915() {
        let mut ui = crate::ui::testing::Fake::typing(&["n", "2"]);
        let choice = ask_for_radio(&mut ui).unwrap();
        assert_eq!(choice, RadioChoice::Preset(radio::preset("us915").unwrap()));
        let said = ui.transcript();
        assert!(said.contains("1) eu868 (ReticulumNet consensus)"), "{said}");
        assert!(
            said.contains("2) us915 (US community — see FCC note)"),
            "{said}"
        );
        assert!(said.contains("3) au915"), "{said}");
        assert!(said.contains("4) custom"), "{said}");
        assert!(said.contains("preset [1]"), "{said}");
    }

    #[test]
    fn an_empty_answer_at_the_menu_takes_the_preselected_eu868() {
        // "n" opens the menu, Enter takes the pre-selection.
        let mut ui = crate::ui::testing::Fake::typing(&["n"]);
        assert_eq!(
            ask_for_radio(&mut ui).unwrap(),
            RadioChoice::Preset(default_preset())
        );
    }

    #[test]
    fn a_menu_answer_that_is_not_an_option_is_asked_again() {
        let mut ui = crate::ui::testing::Fake::typing(&["n", "5", "3"]);
        assert_eq!(
            ask_for_radio(&mut ui).unwrap(),
            RadioChoice::Preset(radio::preset("au915").unwrap())
        );
        let said = ui.transcript();
        assert!(said.contains("not one of the options"), "{said}");
    }

    #[test]
    fn the_us915_caveat_is_said_when_the_menu_picks_it() {
        let mut ui = crate::ui::testing::Fake::typing(&["n", "2"]);
        let opts = Options::default(); // RadioPlan::Ask
        let choice = resolve_radio_choice(&mut ui, &opts).unwrap().unwrap();
        assert_eq!(choice, RadioChoice::Preset(radio::preset("us915").unwrap()));
        assert!(
            ui.transcript().contains("15.247(a)(2)"),
            "{}",
            ui.transcript()
        );
    }

    #[test]
    fn the_us915_caveat_is_said_when_the_flag_picked_it() {
        // The --radio-preset us915 path arrives here as a Fixed plan; the
        // note has to reach the operator on this path too, not only from the
        // menu.
        let mut ui = crate::ui::testing::Fake::agreeing();
        let opts = Options {
            radio: RadioPlan::Fixed(RadioChoice::Preset(radio::preset("us915").unwrap())),
            ..Options::default()
        };
        resolve_radio_choice(&mut ui, &opts).unwrap().unwrap();
        let said = ui.transcript();
        assert!(said.contains("15.247(a)(2)"), "{said}");
        assert!(said.contains("ensure this is lawful"), "{said}");
    }

    #[test]
    fn presets_without_a_caveat_say_nothing_and_skip_resolves_to_nothing() {
        let mut ui = crate::ui::testing::Fake::agreeing();
        let opts = Options {
            radio: RadioPlan::Fixed(RadioChoice::Preset(radio::preset("eu868").unwrap())),
            ..Options::default()
        };
        resolve_radio_choice(&mut ui, &opts).unwrap().unwrap();
        assert!(ui.transcript().is_empty(), "{}", ui.transcript());

        let opts = Options {
            radio: RadioPlan::Skip,
            ..Options::default()
        };
        assert_eq!(resolve_radio_choice(&mut ui, &opts).unwrap(), None);
    }

    #[test]
    fn declining_the_defaults_asks_for_each_field_and_offers_the_eu_value() {
        let mut ui =
            crate::ui::testing::Fake::typing(&["n", "4", "867100000", "250000", "9", "7", "14"]);
        let choice = ask_for_radio(&mut ui).unwrap();
        assert_eq!(
            choice,
            RadioChoice::Custom(RadioSettings {
                frequency_hz: 867_100_000,
                bandwidth_hz: 250_000,
                sf: 9,
                cr: 7,
                tx_power_dbm: 14,
            })
        );
        let said = ui.transcript();
        for prompt in [
            "frequency (Hz) [869463000]",
            "bandwidth (Hz) [125000]",
            "spreadingfactor [8]",
            "codingrate [5]",
            "txpower (dBm) [22]",
        ] {
            assert!(said.contains(prompt), "{prompt} not offered:\n{said}");
        }
    }

    #[test]
    fn a_field_left_empty_keeps_the_value_the_prompt_showed() {
        // Only the frequency is stated; everything else is Enter.
        let mut ui = crate::ui::testing::Fake::typing(&["n", "4", "433175000"]);
        assert_eq!(
            ask_for_radio(&mut ui).unwrap(),
            RadioChoice::Custom(RadioSettings {
                frequency_hz: 433_175_000,
                ..radio::EU868
            })
        );
    }

    #[test]
    fn an_impossible_value_is_asked_again_rather_than_sent() {
        let mut ui = crate::ui::testing::Fake::typing(&[
            "n",
            "4",
            "869525000",
            "100000", // not an SX1262 bandwidth
            "125000",
            "13", // no such spreading factor
            "12",
            "5",
            "99", // beyond the PA
            "22",
        ]);
        let choice = ask_for_radio(&mut ui).unwrap();
        assert_eq!(
            choice,
            RadioChoice::Custom(RadioSettings {
                frequency_hz: 869_525_000,
                bandwidth_hz: 125_000,
                sf: 12,
                cr: 5,
                tx_power_dbm: 22,
            })
        );
        let said = ui.transcript();
        assert!(said.contains("not a bandwidth"), "{said}");
        assert!(said.contains("SF13 is outside"), "{said}");
        assert!(said.contains("99 dBm is outside"), "{said}");
        // The rejected value must not have half-landed: the re-prompt offers
        // the value that is still in force, not the one just refused.
        assert!(said.contains("spreadingfactor [8]"), "{said}");
    }

    // -----------------------------------------------------------------
    // What the operator is told about telemetry (Codeberg #236)
    // -----------------------------------------------------------------

    fn station_target() -> leviculum_core::envelope::TelemetryTargetWire {
        leviculum_core::envelope::TelemetryTargetWire {
            profile: leviculum_core::envelope::TELEMETRY_PROFILE_STATION,
            dest_hash: [0xA7; 16],
            public_key: None,
        }
    }

    #[test]
    fn an_acked_hash_only_target_is_reported_with_what_the_node_does_next() {
        // The operator wants to know it worked. The ack says the board took
        // the frame; the node's own state line says whether it can send yet,
        // and hash-only means "not until an announce answers".
        let mut ui = crate::ui::testing::Fake::agreeing();
        report_telemetry(&mut ui, "3-2.4", &station_target(), SessionReply::Acked);
        let said = ui.transcript();
        assert!(said.contains("telemetry on"), "{said}");
        assert!(said.contains("profile=station"), "{said}");
        assert!(said.contains("state=awaiting-key"), "{said}");
        // And it names where that line can be read, rather than implying
        // lnflash read it back.
        assert!(said.contains("debug port (if00)"), "{said}");
    }

    #[test]
    fn a_target_that_carried_its_key_is_reported_as_ready_instead() {
        let mut ui = crate::ui::testing::Fake::agreeing();
        let with_key = leviculum_core::envelope::TelemetryTargetWire {
            public_key: Some([0x5E; 64]),
            ..station_target()
        };
        report_telemetry(&mut ui, "3-2.4", &with_key, SessionReply::Acked);
        let said = ui.transcript();
        assert!(said.contains("state=ready"), "{said}");
        assert!(!said.contains("awaiting-key"), "{said}");
    }

    #[test]
    fn clearing_the_target_is_reported_as_telemetry_off() {
        let mut ui = crate::ui::testing::Fake::agreeing();
        report_telemetry(
            &mut ui,
            "3-2.4",
            &telemetry::clear_target(),
            SessionReply::Acked,
        );
        let said = ui.transcript();
        assert!(said.contains("telemetry off"), "{said}");
        assert!(!said.contains("awaiting-key"), "{said}");
    }

    #[test]
    fn a_board_that_cannot_take_a_target_is_told_apart_from_one_that_would_not() {
        // Three different facts, three different sentences: the operator has
        // to know whether to reflash, to retry, or to fix the value.
        for (reply, expected) in [
            (SessionReply::NoEnvelope, "predates the control envelope"),
            (SessionReply::NotAccepted, "no telemetry consumer"),
            (SessionReply::NoAnswer, "did not answer"),
            (
                SessionReply::Refused(leviculum_core::envelope::REFUSE_VALUE),
                "refused the telemetry target",
            ),
        ] {
            let mut ui = crate::ui::testing::Fake::agreeing();
            report_telemetry(&mut ui, "3-2.4", &station_target(), reply);
            let said = ui.transcript();
            assert!(said.contains(expected), "{reply:?}: {said}");
            assert!(!said.contains("telemetry on"), "{reply:?}: {said}");
        }
    }

    #[test]
    fn the_flash_flow_asks_about_telemetry_by_default_and_the_answer_is_no() {
        let opts = Options::default();
        assert_eq!(
            opts.telemetry,
            TelemetryPlan::Ask {
                profile: leviculum_core::envelope::TELEMETRY_PROFILE_STATION
            }
        );
        let mut ui = crate::ui::testing::Fake::agreeing();
        assert_eq!(telemetry::resolve(&mut ui, &opts.telemetry).unwrap(), None);
    }

    #[test]
    fn a_dry_run_will_not_even_reboot_a_board_into_its_bootloader() {
        let (_dir, catalogue, manifest) = bundle();
        let mut ui = crate::ui::testing::Fake::agreeing();
        let opts = Options {
            dry_run: true,
            ..Options::default()
        };
        // Both candidates are reported; the application-mode one stops
        // before the touch, and the bootloader one stops at Drive::open,
        // which needs root. Neither writes.
        let outcomes = run(&catalogue, &manifest, &sysfs(), &mut ui, &opts).unwrap();
        assert!(outcomes.is_empty());
        let said = ui.transcript();
        assert!(said.contains("would enter the bootloader"));
        assert!(said.contains("rebooting a board is already a change"));
        assert!(ui.answers.len() == 8, "nothing should have been confirmed");
    }
}
