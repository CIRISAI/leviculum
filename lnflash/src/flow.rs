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
//! way to call them with a board guessed from an application's USB ID.
//!
//! A serial-DFU bootloader publishes no drive and so no `INFO_UF2.TXT`
//! (Codeberg #452). For a `dfu-serial` board the identity is the
//! bootloader's own USB ID, matched by `identify_dfu` against a
//! catalogue that will not load unless that ID belongs to the one board.
//! That is the bootloader speaking, not the firmware that happened to run.
//!
//! With several devices attached, each is resolved individually. "The one
//! UF2 drive" is an assumption, and it is the assumption that went wrong.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::dfu;
use crate::entry;
use crate::envelope::SessionReply;
use crate::infouf2::InfoUf2;
use crate::manifest::{
    self, Board, Catalogue, Flashing, Level, Manifest, Payload, Payloads, Transport,
};
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
    #[error(
        "the board on {port} says it is {board_id:?}, and that Board-ID is published by {}. \
         Only the first takes the {board} image, and the others drive the wrong pins with it. \
         Nothing was written. If this is a {}, pass --board {board}.",
        models.join(", "),
        models.first().map(String::as_str).unwrap_or("board that entry describes")
    )]
    AmbiguousBoardId {
        port: String,
        board_id: String,
        board: String,
        models: Vec<String>,
    },
    #[error(
        "the board on {port} is {board}, which lnflash knows at level {level}: its wiring was \
         checked against the vendor's reference, but we have never run this hardware. Writing \
         it needs the word yes typed at that question, by a person at the prompt or piped in; \
         --yes answers every other confirmation and deliberately not this one. Nothing was \
         written{left}"
    )]
    NeedsConfirmation {
        port: String,
        board: String,
        level: &'static str,
        left: Left,
    },
    #[error("the board on {port} never appeared in its bootloader")]
    NoBootloader { port: String },
    #[error(
        "the board on {port} is running on {id}, which the catalogue gives to {}, and of those \
         only {} is named with --board. Only its bootloader could say which it is, and no \
         bootloader lnflash knows can be sent back to its application from here, so it was not \
         touched. Nothing was written. Double-tap RESET to put it in its bootloader yourself \
         and run this again.",
        claimants.join(", "),
        named.join(", ")
    )]
    SharedRunningId {
        port: String,
        id: UsbId,
        claimants: Vec<String>,
        named: Vec<String>,
    },
    #[error(
        "this run touched the board on {port} into its bootloader, on a running ID the \
         catalogue gives to {hint}, and the bootloader says it is a board not named with \
         --board. Nothing was written, and no bootloader lnflash knows can be sent back to its \
         application from here: press RESET once, or power-cycle it, and it starts what it ran \
         before."
    )]
    TouchedNotNamed { port: String, hint: String },
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
    #[error(
        "{board} still reports SoftDevice {found} after the remedy was written, and it needs \
         {req}. The application was NOT written — writing it onto the wrong SoftDevice produces \
         a board that goes dark. The board is in its bootloader and can be flashed again."
    )]
    RemedyDidNotTake {
        board: String,
        req: String,
        found: String,
    },
    #[error("{0}")]
    Dfu(#[from] dfu::Error),
    #[error(
        "the bootloader on {port} answers on {id}, which no dfu-serial board in the catalogue \
         claims. Nothing was written."
    )]
    UnknownBootloader { port: String, id: UsbId },
    #[error("the bootloader on {port} offers no serial port to send the image over")]
    NoDfuPort { port: String },
    #[error("{board}: its catalogue entry states no serial-DFU parameters. Nothing was written.")]
    NoDfuParams { board: String },
    #[error("serial DFU did not finish: {source}.{}", dfu_consequence(.source, .params))]
    DfuFailed {
        port: String,
        source: dfu::Error,
        params: dfu::Params,
    },
    #[error("cancelled")]
    Cancelled,
}

/// Where a board refused at the level question was left, said at the end of
/// [`Error::NeedsConfirmation`] (864): a person who said no, or a run under
/// `--yes`, has to know whether the board still runs what it ran before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Left {
    /// Asked while it still ran, so nothing was sent to it.
    Running,
    /// Found in its bootloader, and still there.
    InBootloader,
    /// Touched into its bootloader first, because its running ID is shared
    /// by entries at different levels and only the bootloader could say
    /// which board, and so which level, it is.
    Touched,
}

impl std::fmt::Display for Left {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Left::Running => ", and it was not touched: it is still running what it ran before.",
            Left::InBootloader => ", and it is still in its bootloader.",
            Left::Touched => {
                ". This run touched it into its bootloader, since its running ID did not say \
                 which board it is and so not which level: press RESET once, or power-cycle \
                 it, and it starts what it ran before."
            }
        })
    }
}

/// What a failed transfer leaves behind, said after the failure itself.
///
/// The bootloader erases the application area on the start packet and only
/// then reads the init packet (`dfu_single_bank.c`, `dfu_start_pkt_handle`),
/// so every failure after an acknowledged start leaves a board with no
/// application — in its bootloader, and flashable again. A bootloader that
/// refuses the init packet resets the moment it has read it, which from the
/// host is a port that goes away; so does a pulled cable, and nothing on the
/// wire tells the two apart, which is why both are named.
fn dfu_consequence(source: &dfu::Error, params: &dfu::Params) -> String {
    let Some(stage) = source.stage() else {
        return String::new();
    };
    let mut text = String::new();
    if matches!(source, dfu::Error::Gone { .. }) {
        let sd: Vec<String> = params
            .sd_req
            .iter()
            .map(|fwid| format!("{fwid:#06x}"))
            .collect();
        text.push_str(&format!(
            " A bootloader that refuses the init packet resets right after acknowledging it, \
             and that looks exactly like this from here: it takes device type {:#06x} and a \
             SoftDevice among {} only. So does a cable pulled mid-image.",
            params.device_type,
            sd.join(",")
        ));
    }
    if stage.erased_before() {
        text.push_str(
            " The start packet had already erased the application area, so the board stays in \
             its bootloader with no application until a flash goes through; the bootloader and \
             the SoftDevice beneath it are untouched. Run lnflash again; if it stops at \
             the same place twice, the bootloader is refusing the package rather than losing it.",
        );
    } else {
        text.push_str(
            " The start packet was not acknowledged, so the application area may or may not \
             have been erased. Run lnflash again.",
        );
    }
    text
}

/// A board identity read off a mounted bootloader drive.
///
/// The private field is the whole point: this cannot be constructed from an
/// application's USB ID, a command-line flag, or a guess. Only
/// [`confirm_identity`] makes one, from an `INFO_UF2.TXT`, and
/// `identify_dfu` for a bootloader that publishes none, from the
/// bootloader USB ID the catalogue holds exclusive to one `dfu-serial` board.
#[derive(Debug, Clone, Copy)]
pub struct Confirmed<'m> {
    name: &'m str,
    board: &'m Board,
    flashing: &'m Flashing,
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

    /// The flash window, the `Board-ID` and the SoftDevice constraint. A
    /// `Confirmed` carries the flashing half directly rather than an
    /// `Option` of it: a board without one has no `Board-ID` for
    /// [`confirm_identity`] to have matched, so by the time this exists the
    /// half is known to be there.
    pub fn flashing(&self) -> &'m Flashing {
        self.flashing
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
///
/// A match is not always the whole decision (Codeberg #262). Where the
/// catalogue says the `Board-ID` is shared by several models, the match is a
/// hint, and it is written only under `--board` or after the person at the
/// prompt names the board. Where the catalogue's level is `expected`, the
/// person has to say yes to a question stating that we have never run that
/// hardware. Both go through [`Ui::ask`], which answers nothing under
/// `--yes`, so an unattended run refuses rather than consents.
pub fn confirm_identity<'m>(
    catalogue: &'m Catalogue,
    manifest: &'m Manifest,
    info: &InfoUf2,
    port: &str,
    asked_for: Option<&str>,
    ui: &mut dyn Ui,
) -> Result<Confirmed<'m>, Error> {
    confirm_identity_after(catalogue, manifest, info, port, asked_for, &[], None, ui)
}

/// [`confirm_identity`] for a board whose level question may already have
/// been answered before the touch ([`level_before_touch`]): `answered`
/// names the boards a yes was typed for, and the bootloader naming one of
/// them is not asked again. One it names otherwise is asked as usual.
///
/// `running` is the board a leviculum image named before the touch
/// ([`running_ours`]). It settles a shared Board-ID only when the
/// bootloader's identity already points at that very board, so it never
/// decides what a board is, only which of the models behind one bootloader
/// it is.
#[allow(clippy::too_many_arguments)]
fn confirm_identity_after<'m>(
    catalogue: &'m Catalogue,
    manifest: &'m Manifest,
    info: &InfoUf2,
    port: &str,
    asked_for: Option<&str>,
    answered: &[String],
    running: Option<&str>,
    ui: &mut dyn Ui,
) -> Result<Confirmed<'m>, Error> {
    let confirmed = resolve_identity(catalogue, manifest, info, port, asked_for)?;
    let name = confirmed.name();
    let identify = &confirmed.flashing().identify;
    if identify.board_id_shared && asked_for.is_none() && running == Some(name) {
        ui.say(&format!(
            "{port}: Board-ID {:?} is shared, and this board was running our {name} image,              which was written to it as a {name}: no question.",
            identify.info_uf2_board_id
        ));
    } else if identify.board_id_shared && asked_for.is_none() {
        let models = &identify.board_id_models;
        let ours = models.first().map(String::as_str).unwrap_or(name);
        let answer = ui.ask(&format!(
            "{port}: Board-ID {:?} is published by {}, and only the first takes the {name} \
             image. Type {name} if this is a {ours}:",
            identify.info_uf2_board_id,
            models.join(", ")
        ))?;
        if !answer.is_some_and(|a| a.trim().eq_ignore_ascii_case(name)) {
            return Err(Error::AmbiguousBoardId {
                port: port.to_string(),
                board_id: identify.info_uf2_board_id.clone(),
                board: name.to_string(),
                models: models.clone(),
            });
        }
    }
    ask_about_level(&confirmed, port, answered, ui)?;
    Ok(confirmed)
}

/// The question a board at level `expected` puts to the person, whichever
/// way its identity was established, unless `answered` says it was put to
/// them before the touch.
fn ask_about_level(
    confirmed: &Confirmed,
    port: &str,
    answered: &[String],
    ui: &mut dyn Ui,
) -> Result<(), Error> {
    let name = confirmed.name();
    if answered.iter().any(|a| a == name) {
        return Ok(());
    }
    put_level_question(
        port,
        name,
        confirmed.flashing().level,
        Left::InBootloader,
        ui,
    )
}

/// The level question itself, and the refusal a missing yes comes to.
/// `left` is where a refusal leaves the board.
fn put_level_question(
    port: &str,
    name: &str,
    level: Level,
    left: Left,
    ui: &mut dyn Ui,
) -> Result<(), Error> {
    if level != Level::Expected {
        return Ok(());
    }
    let answer = ui.ask(&format!(
        "{port}: {name} is at level {}: its wiring matches the vendor's reference, but we \
         have never run this hardware. Type yes to write it anyway:",
        level.as_str()
    ))?;
    if !answer.is_some_and(|a| a.trim().eq_ignore_ascii_case("yes")) {
        return Err(Error::NeedsConfirmation {
            port: port.to_string(),
            board: name.to_string(),
            level: level.as_str(),
            left,
        });
    }
    Ok(())
}

/// Identify for a bootloader that publishes no `INFO_UF2.TXT`: the
/// `dfu-serial` board whose bootloader answers on `id` (Codeberg #452).
///
/// A board `--board` does not name is skipped with `Ok(None)` and a line
/// saying so, as [`confirm_named`] skips one. The level question is not put
/// here but by [`inspect_dfu`], the only caller, after it has shown the
/// plan: a dry run then shows the port and the package even for a board
/// whose question would go unanswered (838), and a person answers it with
/// the plan in front of them.
fn identify_dfu<'m>(
    catalogue: &'m Catalogue,
    manifest: &'m Manifest,
    id: UsbId,
    port: &str,
    named: &[String],
    ui: &mut dyn Ui,
) -> Result<Option<Confirmed<'m>>, Error> {
    let Some((name, board, flashing)) = catalogue.board_for_bootloader_usb(id) else {
        return Err(Error::UnknownBootloader {
            port: port.to_string(),
            id,
        });
    };
    if !named.is_empty() && !named.iter().any(|n| n == name) {
        ui.say(&format!(
            "{port}: bootloader {id} is {name}, which is not among the boards named with --board \
             ({}). Skipped: nothing was written, and it is still in its bootloader.\n",
            named.join(", ")
        ));
        return Ok(None);
    }
    let confirmed = Confirmed {
        name,
        board,
        flashing,
        payloads: manifest.payloads(name)?,
        _private: (),
    };
    Ok(Some(confirmed))
}

/// [`confirm_identity`] against the boards a run named with `--board`
/// (Codeberg #262).
///
/// The list says which models are present, so a shared Board-ID whose entry
/// is in it is decided without a question, which is the one way through for
/// an unattended run on boards one owns. A board whose entry is not in it is
/// skipped with a line saying so, and `Ok(None)` comes back: nothing is
/// written to it and it is not a refusal. With no board named this is
/// exactly [`confirm_identity`] with nothing asked for.
///
/// The decision is taken on the bootloader's `Board-ID`, never on the USB ID
/// the board was found by, so a skipped board has already been rebooted into
/// its bootloader and stays there.
pub fn confirm_named<'m>(
    catalogue: &'m Catalogue,
    manifest: &'m Manifest,
    info: &InfoUf2,
    port: &str,
    named: &[String],
    answered: &[String],
    ui: &mut dyn Ui,
) -> Result<Option<Confirmed<'m>>, Error> {
    confirm_named_running(catalogue, manifest, info, port, named, answered, None, ui)
}

/// [`confirm_named`] with the board a running leviculum image named before
/// the touch ([`running_ours`]).
#[allow(clippy::too_many_arguments)]
fn confirm_named_running<'m>(
    catalogue: &'m Catalogue,
    manifest: &'m Manifest,
    info: &InfoUf2,
    port: &str,
    named: &[String],
    answered: &[String],
    running: Option<&str>,
    ui: &mut dyn Ui,
) -> Result<Option<Confirmed<'m>>, Error> {
    if named.is_empty() {
        return confirm_identity_after(
            catalogue, manifest, info, port, None, answered, running, ui,
        )
        .map(Some);
    }
    // An ID that is missing or unknown falls through to confirm_identity,
    // which names the problem; only a board we can name can be skipped.
    let found = info
        .board_id()
        .map(str::trim)
        .and_then(|id| catalogue.board_for_id(id))
        .map(|(name, _, _)| name);
    match found {
        Some(name) if !named.iter().any(|n| n == name) => {
            ui.say(&format!(
                "{port}: Board-ID {:?} is {name}, which is not among the boards named with \
                 --board ({}). Skipped: nothing was written, and it is still in its \
                 bootloader.\n",
                info.board_id().map(str::trim).unwrap_or(""),
                named.join(", ")
            ));
            Ok(None)
        }
        found => confirm_identity_after(catalogue, manifest, info, port, found, answered, None, ui)
            .map(Some),
    }
}

/// The match alone, with no question asked. [`confirm_identity`] is the way
/// in for a session; this is also what re-confirms a drive that reappeared
/// after the SoftDevice remedy, where the person has already answered for
/// this board in this session and asking again would only teach them to
/// answer without reading.
fn resolve_identity<'m>(
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
    // Only a flashable board can come back from here, and the boards listed
    // when none does are the flashable ones: naming a control-only board in
    // this message would send the user looking for an image that no bundle
    // is allowed to carry.
    let Some((name, board, flashing)) = catalogue.board_for_id(board_id) else {
        return Err(Error::UnknownBoard {
            port: port.to_string(),
            board_id: board_id.to_string(),
            available: catalogue
                .flashable_names()
                .iter()
                .map(|s| s.to_string())
                .collect(),
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
        flashing,
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

/// The post-remedy gate: what the board reports *after* the SoftDevice write
/// decides whether the application may be written (#278).
///
/// Separate from [`check_softdevice`] because the two failures are different
/// facts. Before the remedy, an unmet precondition means "install the
/// SoftDevice first". After it, it means the write did not take, and the one
/// thing that must not follow is our application landing on the wrong
/// SoftDevice — the board goes dark and the operator has no way back except a
/// bootloader they can no longer reach the same way.
pub fn remedy_took(after: &Installed, req: Option<&VersionReq>, board: &str) -> Result<(), Error> {
    if check_softdevice(after, req) == Precondition::Met {
        return Ok(());
    }
    Err(Error::RemedyDidNotTake {
        board: board.to_string(),
        req: req.map(|r| r.as_str().to_string()).unwrap_or_default(),
        found: after.describe(),
    })
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
    prepare_for(payload, root, confirmed.name(), confirmed.flashing())
}

/// [`prepare`] against a catalogue entry rather than a confirmed board: what
/// a dry run that has not entered the bootloader can still check.
fn prepare_for(
    payload: &Payload,
    root: &Path,
    name: &str,
    flashing: &Flashing,
) -> Result<Image, Error> {
    // The only way to get the bytes, and it verifies the checksum.
    let bytes = payload.read(root)?;
    let image = match payload.convert.unwrap_or(manifest::Convert::None) {
        manifest::Convert::None => Image::parse(&bytes)?,
        manifest::Convert::HexToUf2 => {
            let text = String::from_utf8_lossy(&bytes);
            Image::from_spans(&crate::ihex::parse(&text)?, flashing.flash.family_id)
        }
    };
    let file = payload.file.display().to_string();

    if let Some(actual) = image.family_id() {
        if actual != flashing.flash.family_id {
            return Err(Error::WrongFamily {
                file,
                board: name.to_string(),
                actual,
                expected: flashing.flash.family_id,
            });
        }
    }
    // The upper bound is the one that matters: at or above `writable_end`
    // the bootloader rejects outright. Below `writable_start` blocks are
    // declined silently, which is expected for a SoftDevice image carrying
    // an MBR, so a low start is reported, not refused.
    let (low, high) = image.address_range().unwrap_or((0, 0));
    if high > flashing.flash.writable_end {
        return Err(Error::OutsideWindow {
            file,
            board: name.to_string(),
            low,
            high,
            start: flashing.flash.writable_start,
            end: flashing.flash.writable_end,
        });
    }
    Ok(image)
}

/// The serial-DFU package for a board's application payload: the image
/// checked as [`prepare`] checks it, made into the bytes `adafruit-nrfutil
/// dfu genpkg` would make of it, with the init packet the catalogue's
/// parameters give (Codeberg #452).
///
/// A hex payload is converted straight from its records, which is the
/// reference's own conversion byte for byte; a UF2 payload from its blocks,
/// which differs from that only in the zero padding a UF2 block carries.
fn dfu_package(
    payload: &Payload,
    root: &Path,
    name: &str,
    flashing: &Flashing,
) -> Result<dfu::Package, Error> {
    let params = flashing.dfu_params().ok_or_else(|| Error::NoDfuParams {
        board: name.to_string(),
    })?;
    let image = prepare_for(payload, root, name, flashing)?;
    let spans = match payload.convert.unwrap_or(manifest::Convert::None) {
        manifest::Convert::None => dfu::image_spans(&image),
        manifest::Convert::HexToUf2 => {
            crate::ihex::parse(&String::from_utf8_lossy(&payload.read(root)?))?
        }
    };
    let firmware = dfu::firmware(&spans, flashing.flash.app_base)?;
    Ok(dfu::Package::new(&params, firmware))
}

/// The plan a serial-DFU write shows before it asks, dry run or not.
fn dfu_plan(confirmed: &Confirmed, package: &dfu::Package, tty: &str) -> Vec<String> {
    let flashing = confirmed.flashing();
    let app = &confirmed.payloads().app;
    let sd: Vec<String> = package
        .init
        .sd_req
        .iter()
        .map(|fwid| format!("{fwid:#06x}"))
        .collect();
    vec![
        format!(
            "  send {} ({}) over serial DFU on {tty} at {} baud",
            app.file.display(),
            app.git_sha
                .as_deref()
                .map(|sha| format!("git_sha={sha}"))
                .unwrap_or_else(|| "no build recorded".into()),
            dfu::BAUD
        ),
        format!("  package: {}", package.describe()),
        format!(
            "  which writes {:#x}-{:#x}; the start packet erases that area before anything \
             else is checked",
            flashing.flash.app_base,
            flashing.flash.app_base as usize + package.firmware.len()
        ),
        format!(
            "  the SoftDevice cannot be read over serial DFU: the bootloader refuses the image \
             unless it runs one of {} ({} by the catalogue)",
            sd.join(","),
            flashing
                .requires
                .softdevice
                .as_deref()
                .unwrap_or("no constraint")
        ),
    ]
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
    /// The boards present, by catalogue name (`--board`, repeatable). Empty
    /// means none was named, and every shared Board-ID is asked about. A
    /// board whose entry is not in a non-empty list is left alone.
    pub boards: Vec<String>,
    /// Inspect and report; change nothing, and do not even reboot a board
    /// into its bootloader, which is itself a change to somebody's device.
    /// A dry run writes nothing, so it refuses nothing: an identity a real
    /// run would refuse, or a question it would ask, is said as such and the
    /// board is a skip, so a dry run that completes exits
    /// [`EXIT_CONFIRMED`] (838).
    pub dry_run: bool,
    /// How long to wait for a bootloader or an application to appear.
    pub appear_within: Duration,
    /// How long to spend establishing which build the board is running
    /// once it has come back ([`verify::FRESH_BANNER_BUDGET`]).
    pub banner_budget: Duration,
    /// What to do about the radio configuration once the board is up:
    /// estimate it (the default, [`RadioPlan::Estimate`]), ask, send what the
    /// flags already decided, or leave it alone.
    pub radio: RadioPlan,
    /// What to do about the telemetry target once the board is up (#236).
    /// The flash run's default is to send nothing, the answer "no" to the
    /// question `--ask` puts.
    pub telemetry: TelemetryPlan,
    /// What to do about the remote-management allow-list once the board is
    /// up (#235). The flash run's default is to send nothing, which is the
    /// answer "no" to the question `--ask` puts: an unattended board must
    /// come up serving nobody.
    pub management: crate::mgmt::MgmtPlan,
    /// `--ask`: put every question the zero-config run decides on its own
    /// (#476) — the overwrite, the radio menu, telemetry and remote
    /// management. Off by default: starting lnflash flashes what it finds.
    pub ask: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            boards: Vec::new(),
            dry_run: false,
            appear_within: entry::BOOTLOADER_APPEARS_WITHIN,
            banner_budget: verify::FRESH_BANNER_BUDGET,
            radio: RadioPlan::default(),
            telemetry: TelemetryPlan::Skip,
            management: crate::mgmt::MgmtPlan::Skip,
            ask: false,
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

/// What one board's flash amounts to, in the only three states a caller
/// can act on differently.
///
/// Kept apart because "not confirmed" was reported as a failure and cost a
/// day of doubt on hardware that was fine (Codeberg #378). A script that
/// chains on `lnflash` has to be able to tell "the board contradicts the
/// image I wrote" (write it again, or look at the board) from "I could not
/// read the board back" (read it again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// Written, and the board named the build we wrote.
    Confirmed,
    /// Written, and the board neither confirmed nor contradicted it.
    Unknown,
    /// Not written, or the board contradicted it.
    Failed,
}

impl Outcome {
    /// Whether the firmware is on the board and confirmed.
    ///
    /// Neither the radio configuration nor the telemetry target is part of
    /// this. The flash has happened by the time those steps run, and a board
    /// that did not ACK is a board running our firmware on what it had
    /// stored — worth a warning, not worth reporting the flash as failed.
    pub fn is_good(&self) -> bool {
        self.confirmation() == Confirmation::Confirmed
    }

    /// This board's contribution to the process exit code.
    pub fn confirmation(&self) -> Confirmation {
        if self.application_written.is_none() {
            return Confirmation::Failed;
        }
        match &self.verdict {
            Some(verdict) if verdict.is_confirmed() => Confirmation::Confirmed,
            Some(verdict) if verdict.contradicts() => Confirmation::Failed,
            // No verdict at all is the same claim as an unconfirmed one:
            // the image went to the board and nothing came back about it.
            _ => Confirmation::Unknown,
        }
    }

    /// The closing line for this board, when it is not a plain success.
    pub fn describe(&self) -> String {
        match (&self.application_written, &self.verdict) {
            (None, _) => "nothing was written".to_string(),
            (Some(_), None) => {
                "flashed, and the running build is unknown — it was never read back".to_string()
            }
            (Some(_), Some(verdict)) => verdict.describe(),
        }
    }
}

/// What the radio step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioOutcome {
    pub settings: RadioSettings,
    /// What the board did with the frame.
    pub taken: radio::ConfigTaken,
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
) -> Result<Bus, Error> {
    run_with(catalogue, manifest, sysfs, ui, opts, &Mounted)
}

/// [`run`] with the dry run's drive reads handed in, so a test reaches both
/// sides of the root line whatever uid runs it (838).
fn run_with(
    catalogue: &Catalogue,
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    drives: &dyn BootDrives,
) -> Result<Bus, Error> {
    // `--board` is the only handle a user has for saying which board a run is
    // for, so a name this tool does not flash is answered before the bus is
    // read at all — and the boards offered instead are the flashable ones.
    for asked in &opts.boards {
        match catalogue.board(asked) {
            Ok(board) => board.require_flashing(asked).map(|_| ())?,
            Err(_) => {
                return Err(manifest::Error::UnknownBoard {
                    wanted: asked.clone(),
                    available: catalogue
                        .flashable_names()
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                }
                .into())
            }
        }
    }

    let candidates = find_candidates(catalogue, sysfs)?;
    for stranger in strangers(sysfs, &candidates)? {
        ui.say(&describe_stranger(&stranger));
    }
    if candidates.is_empty() {
        ui.say("No board lnflash knows is attached.");
        // The flashable ones, not every entry: this is a flashing session,
        // and a user sent after a control-only board would be looking for an
        // image no bundle may carry.
        ui.say(&format!(
            "It flashes: {}. Nothing to do.",
            catalogue.flashable_names().join(", ")
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
        return Ok(Bus::default());
    }

    ui.say(&format!("Found {} device(s):", candidates.len()));
    for candidate in &candidates {
        ui.say(&candidate.describe());
    }
    ui.say("");

    Ok(each_candidate(&candidates, ui, |ui, candidate| {
        resolve(catalogue, manifest, sysfs, ui, opts, drives, candidate)
    }))
}

/// The serial devices on the bus that no catalogue entry claims: an ESP32
/// RNode, a ThinkNode M9, a GPS mouse, anything with a tty and a USB ID
/// lnflash does not know. Each gets one line and nothing else (#476): a run
/// that passed over them in silence would look exactly like one that never
/// saw them, and the person looking at a board on the desk deserves to know
/// which of the two happened. Hubs and every other device without a tty are
/// not strangers worth a line.
fn strangers(sysfs: &Sysfs, candidates: &[Candidate]) -> io::Result<Vec<Device>> {
    Ok(sysfs
        .devices()?
        .into_iter()
        .filter(|device| !candidates.iter().any(|c| c.device.name == device.name))
        .filter(|device| device.interfaces.iter().any(|i| i.tty.is_some()))
        .collect())
}

fn describe_stranger(device: &Device) -> String {
    format!(
        "  {} [{}] {}: not a board lnflash flashes; left alone, nothing was sent to it.",
        device.name,
        device.id,
        device.product.as_deref().unwrap_or("unnamed device")
    )
}

/// What a run did to every board on the bus.
///
/// Refused and skipped boards are kept apart from the outcomes and from each
/// other because they mean different things to the exit code: a refused
/// board was put in its bootloader, or was meant to be written, and nothing
/// was written to it, which fails the run; a skipped board is one this run
/// was not for (Codeberg #262).
#[derive(Debug, Clone, Default)]
pub struct Bus {
    /// The boards that reached the write.
    pub outcomes: Vec<Outcome>,
    /// Ports of the boards whose step ended in an error: an identity that
    /// was refused, a Board-ID the catalogue does not know, a write that
    /// never started.
    pub refused: Vec<String>,
    /// Ports of the boards left alone on purpose: not named with `--board`,
    /// not flashed by this tool, or stopped by `--dry-run`.
    pub skipped: Vec<String>,
}

/// Every board was written and named the build in this bundle.
pub const EXIT_CONFIRMED: u8 = 0;
/// The flash failed: a board did not come back, or came back naming a
/// different build, or nothing was written to it, which includes a board
/// whose identity was refused or whose Board-ID is not in the catalogue. A
/// board skipped because the run was not for it does not count (#262).
pub const EXIT_FLASH_FAILED: u8 = 1;
/// Every board took the write and none contradicted it, and at least one
/// could not be read back. Separate from [`EXIT_FLASH_FAILED`] because the
/// two need different things done about them, and because collapsing them
/// is what made a good flash stop a script (Codeberg #378).
pub const EXIT_UNCONFIRMED: u8 = 2;

impl Bus {
    /// Whether no board was refused, written or skipped.
    pub fn is_empty(&self) -> bool {
        self.outcomes.is_empty() && self.refused.is_empty() && self.skipped.is_empty()
    }

    /// The process exit code of a flash run. The worst outcome decides, and
    /// a contradiction is worse than an absence of evidence. A refused board
    /// is a board left in its bootloader with nothing written, so it fails
    /// the run however the others went; a skipped one does not count.
    pub fn exit_code(&self) -> u8 {
        if !self.refused.is_empty() {
            return EXIT_FLASH_FAILED;
        }
        let mut code = EXIT_CONFIRMED;
        for outcome in &self.outcomes {
            match outcome.confirmation() {
                Confirmation::Confirmed => {}
                Confirmation::Unknown => code = code.max(EXIT_UNCONFIRMED),
                Confirmation::Failed => return EXIT_FLASH_FAILED,
            }
        }
        code
    }
}

/// The loop over the bus, apart from what is done to each board so a test
/// can drive it with boards that are not there.
///
/// One board's refusal must not abandon the others: with several devices
/// attached each is its own decision. A refused board is said and its port
/// kept in [`Bus::refused`], so it fails the run without stopping the boards
/// after it ([`Bus::exit_code`]).
fn each_candidate(
    candidates: &[Candidate],
    ui: &mut dyn Ui,
    mut step: impl FnMut(&mut dyn Ui, &Candidate) -> Result<Option<Outcome>, Error>,
) -> Bus {
    let mut bus = Bus::default();
    for candidate in candidates {
        let port = &candidate.device.name;
        match step(ui, candidate) {
            Ok(Some(outcome)) => bus.outcomes.push(outcome),
            Ok(None) => bus.skipped.push(port.clone()),
            Err(err) => {
                ui.say(&format!("{port}: {err}\n"));
                bus.refused.push(port.clone());
            }
        }
    }
    bus
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
    open_interface(
        sysfs,
        device,
        tty,
        radio::TRANSPORT_INTERFACE,
        "transport port",
        "nothing was sent",
    )
    .map(|(fd, _)| fd)
}

/// [`open_transport`] for the debug CDC (if00): same proof, and DTR+RTS
/// raised the same way — the debug port transmits only with both set.
pub(crate) fn open_debug(sysfs: &Sysfs, device: &Device, tty: &Path) -> io::Result<crate::sys::Fd> {
    open_debug_proven(sysfs, device, tty).map(|(fd, _)| fd)
}

/// The debug port, and the device node the open was proven against.
///
/// Same open and same proof as [`open_debug`]. The node comes back because
/// a build claim read on this fd has to be able to say where it was read
/// (Codeberg #378): a sha with no port beside it left the 2026-09-11
/// recurrence undecidable from the tool's output alone.
pub(crate) fn open_debug_proven(
    sysfs: &Sysfs,
    device: &Device,
    tty: &Path,
) -> io::Result<(crate::sys::Fd, std::path::PathBuf)> {
    open_interface(
        sysfs,
        device,
        tty,
        crate::watch::DEBUG_INTERFACE,
        "debug port",
        "nothing was read",
    )
}

fn open_interface(
    sysfs: &Sysfs,
    device: &Device,
    tty: &Path,
    interface: u8,
    what: &str,
    consequence: &str,
) -> io::Result<(crate::sys::Fd, std::path::PathBuf)> {
    let fd = crate::sys::Fd::open_serial(tty)?;
    if interface == crate::watch::DEBUG_INTERFACE {
        fd.set_debug_port()?;
    } else {
        fd.set_transport_port()?;
    }
    let Some(current) = sysfs
        .devices()?
        .into_iter()
        .find(|d| d.is_same_board(device))
    else {
        return Err(io::Error::other(format!(
            "{} is no longer on the bus; {consequence}",
            device.name
        )));
    };
    let Some(tty_name) = current.interface(interface).and_then(|i| i.tty.clone()) else {
        return Err(io::Error::other(format!(
            "{} has no {what} (if{interface:02}) any more; {consequence}",
            current.name
        )));
    };
    let expected = sysfs.dev_path(&tty_name);
    let expected_rdev = std::os::unix::fs::MetadataExt::rdev(&std::fs::metadata(&expected)?);
    if fd.rdev()? != expected_rdev {
        return Err(io::Error::other(format!(
            "{} re-enumerated: its {what} is {} now, not {}; {consequence}",
            current.name,
            expected.display(),
            tty.display()
        )));
    }
    let holders = sysfs.holders(expected_rdev);
    if !holders.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::ResourceBusy,
            held_port(tty, &holders, consequence),
        ));
    }
    Ok((fd, expected))
}

/// The sentence for a port another program holds (#476). lnsd is named on
/// its own because its serial medium opens the transport port of every
/// LNode it finds (`leviculum-std/src/auto_serial.rs`), so a board flashed
/// on a host running lnsd is held again the moment it comes back.
fn held_port(tty: &Path, holders: &[crate::usb::Holder], consequence: &str) -> String {
    let who = holders
        .iter()
        .map(|h| format!("{} (pid {})", h.name, h.pid))
        .collect::<Vec<_>>()
        .join(", ");
    let remedy = if holders.iter().any(|h| h.name == "lnsd") {
        "stop lnsd or let it go of this board (its serial medium opens every LNode's \
         transport port), then run lnflash again"
    } else {
        "close it, then run lnflash again"
    };
    format!(
        "{} is held by {who}: two programs on one port each get half of the answers, so \
         {consequence}. To use it, {remedy}",
        tty.display()
    )
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
            SessionReply::ProbeSilent => ui.say(&format!(
                "{port}: the board did not answer the capability probe, so no wall time was \
                 sent. {}",
                crate::envelope::PROBE_SILENCE_HINT
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
            SessionReply::ProbeSilent => ui.say(&format!(
                "{port}: the board did not answer the capability probe, so no spacing was \
                 sent. {}",
                crate::envelope::PROBE_SILENCE_HINT
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

/// The `--announce` session (#376): no flash — find the boards already
/// running and ask each to announce its LXMF delivery destination now,
/// on all its interfaces, exactly as its telemetry path does before a
/// report. One-shot, nothing persisted.
///
/// The bench instrument for the direct-announce loss: it separates "the
/// announce never left the board" from "it left and was not taken"
/// without waiting out the board's own announce cadence. The clock gate
/// is the telemetry path's: a board without a calendar clock withholds,
/// answers the named `no-clock` refusal, and says
/// `[ANNOUNCE] withheld reason=no-clock` on its debug port — the tool
/// repeats that reading rather than reporting a generic refusal.
pub fn announce(catalogue: &Catalogue, sysfs: &Sysfs, ui: &mut dyn Ui) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --announce talks to flashed boards; a board in its \
             bootloader has nothing to announce.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| send_announce_to(&fd))
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
                "{port}: announce sent. The board logs [ANNOUNCE] sent dst=… reason=host and \
                 the usual BLE_TX_PKT lines on its debug port (if00); read those to see which \
                 carriers took it."
            )),
            SessionReply::Refused(reason) => ui.say(&format!(
                "{port}: the board refused the announce — {}.",
                crate::envelope::reason_str(reason)
            )),
            SessionReply::NoAnswer => ui.say(&format!(
                "{port}: the board did not answer the announce frame, so whether it announced \
                 is unknown — read its debug port."
            )),
            SessionReply::ProbeSilent => ui.say(&format!(
                "{port}: the board did not answer the capability probe, so no announce was \
                 requested. {}",
                crate::envelope::PROBE_SILENCE_HINT
            )),
            SessionReply::NotAccepted => ui.say(&format!(
                "{port}: this firmware speaks the envelope but has no announce command. Flash \
                 the current bundle first."
            )),
        }
    }
    Ok(all_took_it)
}

fn send_announce_to(fd: &crate::sys::Fd) -> io::Result<SessionReply> {
    use crate::envelope;
    envelope::probed(fd, leviculum_core::envelope::TYPE_ANNOUNCE, |fd| {
        envelope::send_announce(fd)
    })
}

/// The `--set-ble-tx-gap` session (#376): no flash — find the boards
/// already running and tell each what gap to leave between the last
/// fragment of one packet and the first fragment of the next packet on
/// the same BLE connection.
///
/// The BLE sibling of [`set_tx_spacing`], and shaped like it: a bench
/// instrument, deliberately volatile — the value is not persisted, so a
/// reset or power cycle puts the board back on 0 (no gap). A measurement
/// must not be able to leave a board silently paced after the session
/// that paced it.
pub fn set_ble_tx_gap(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    gap_ms: u16,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-ble-tx-gap talks to flashed boards; a board in \
             its bootloader has no Bluetooth links to pace.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| send_ble_tx_gap_to(&fd, gap_ms))
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
            SessionReply::Acked if gap_ms == 0 => ui.say(&format!(
                "{port}: BLE transmit gap disabled — the board imposes no gap between \
                 packets on a connection until a reset restores the 100 ms default."
            )),
            SessionReply::Acked => ui.say(&format!(
                "{port}: BLE transmit gap set to {gap_ms} ms (compiled default: 100 ms). \
                 The board logs [BLE ] tx_gap_ms={gap_ms} now and BLE_TX_GAP conn=… \
                 waited_ms=… on its debug port (if00) for every packet it defers. Not \
                 persisted: a reset restores the default."
            )),
            SessionReply::Refused(reason) => ui.say(&format!(
                "{port}: the board refused the BLE transmit gap — {}.",
                crate::envelope::reason_str(reason)
            )),
            SessionReply::NoAnswer => ui.say(&format!(
                "{port}: the board did not answer the BLE transmit-gap frame, so it is still \
                 on whatever gap it had."
            )),
            SessionReply::ProbeSilent => ui.say(&format!(
                "{port}: the board did not answer the capability probe, so no gap was sent. {}",
                crate::envelope::PROBE_SILENCE_HINT
            )),
            SessionReply::NotAccepted => ui.say(&format!(
                "{port}: this firmware speaks the envelope but has no BLE transmit-gap knob. \
                 Flash the current bundle first."
            )),
        }
    }
    Ok(all_took_it)
}

fn send_ble_tx_gap_to(fd: &crate::sys::Fd, gap_ms: u16) -> io::Result<SessionReply> {
    use crate::envelope;
    envelope::probed(fd, leviculum_core::envelope::TYPE_BLE_TX_GAP, |fd| {
        envelope::send_ble_tx_gap(fd, gap_ms)
    })
}

/// The `--store-storm` session (#384): no flash — find the boards already
/// running and ask each to append N synthetic records of a given size to its
/// message store.
///
/// The instrument for the erase-storm measurement. A 4 KiB page erase holds the
/// flash for ~85 ms and the SoftDevice has to fit it between radio events, so
/// "what does a store under load cost BLE and LoRa" is a question about erases,
/// and waiting for a mesh to fill 16 pages is not a way to ask it.
///
/// Shaped like [`set_tx_spacing`] and [`set_ble_tx_gap`]: nothing is persisted,
/// and the session ends after the boards have answered. What it does NOT do is
/// wait for the records — the board acks the request and its store task writes
/// them on its own time, which is the whole point of the instrument (the
/// measurement runs during the writing). The board's `STORE storm …` line on
/// if00 is what says how it went.
pub fn store_storm(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    storm: leviculum_core::envelope::StoreStormWire,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --store-storm talks to flashed boards; a board in \
             its bootloader has no store mounted.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| send_store_storm_to(&fd, storm))
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
                "{port}: storm accepted — {} records of {} B are being appended now. The \
                 board logs STORE storm records=… appended=… and one STORE op_fail line per \
                 flash operation the SoftDevice refused, on its debug port (if00). The \
                 records are tagged as synthetic; nothing is persisted as configuration.",
                storm.records, storm.size
            )),
            SessionReply::Refused(reason) => ui.say(&format!(
                "{port}: the board refused the storm — {}. A busy board is either still \
                 running the previous storm or has no store mounted this boot; its STORE \
                 mount line on if00 says which.",
                crate::envelope::reason_str(reason)
            )),
            SessionReply::NoAnswer => ui.say(&format!(
                "{port}: the board did not answer the storm frame, so assume nothing was \
                 appended."
            )),
            SessionReply::ProbeSilent => ui.say(&format!(
                "{port}: the board did not answer the capability probe, so no storm was \
                 sent. {}",
                crate::envelope::PROBE_SILENCE_HINT
            )),
            SessionReply::NotAccepted => ui.say(&format!(
                "{port}: this firmware speaks the envelope but carries no message store. \
                 Flash the current bundle first."
            )),
        }
    }
    Ok(all_took_it)
}

fn send_store_storm_to(
    fd: &crate::sys::Fd,
    storm: leviculum_core::envelope::StoreStormWire,
) -> io::Result<SessionReply> {
    use crate::envelope;
    envelope::probed(fd, leviculum_core::envelope::TYPE_STORE_STORM, |fd| {
        envelope::send_store_storm(fd, storm)
    })
}

/// The `--stamp-cost` / `--peering-cost` session (#384): set the
/// propagation-node costs on every running LNode, then exit.
///
/// Persisted, unlike the bench knobs above: the board merges the frame
/// over its config-page record (a field the CLI did not name keeps its
/// persisted value), writes the page, and only then acks — so the ack
/// means "a reset comes up with this". The running role read its costs
/// at boot; the answer text says so rather than implying a live change.
pub fn set_pn_config(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    wire: leviculum_core::envelope::PnConfigWire,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --stamp-cost/--peering-cost talk to flashed              boards; a board in its bootloader has no config page owner.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| send_pn_config_to(&fd, wire))
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
                "{port}: persisted. Takes effect at the next reset; the boot PN_CONFIG                  line on the debug port (if00) proves what came up."
            )),
            SessionReply::Refused(reason) => ui.say(&format!(
                "{port}: the board refused the costs — {}.",
                crate::envelope::reason_str(reason)
            )),
            SessionReply::NoAnswer => ui.say(&format!(
                "{port}: the board did not answer, so assume nothing was persisted."
            )),
            SessionReply::ProbeSilent => ui.say(&format!(
                "{port}: the board did not answer the capability probe, so nothing was                  sent. {}",
                crate::envelope::PROBE_SILENCE_HINT
            )),
            SessionReply::NotAccepted => ui.say(&format!(
                "{port}: this firmware speaks the envelope but carries no propagation                  node. Flash the current bundle first."
            )),
        }
    }
    Ok(all_took_it)
}

/// The `--share-position` session (order 1301): set whom every running
/// LNode shares its position with, then exit. No flashing.
///
/// Persisted, like the costs, and applied at once: the board writes the
/// record, acks, and its next position announce follows the setting. The
/// board's `[POSITION] share=` line on the debug port says what it now
/// runs, which for `default` depends on whether it reports as a tracker.
pub fn set_position_share(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    setting: u8,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --share-position talks to flashed boards; a board in its bootloader has no config page owner.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty).and_then(|fd| {
            crate::envelope::probed(&fd, leviculum_core::envelope::TYPE_POSITION_SHARE, |fd| {
                crate::envelope::send_position_share(fd, setting)
            })
        }) {
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
                "{port}: persisted and in force. The [POSITION] share= line on the debug port (if00) says whom the board now shares with."
            )),
            SessionReply::Refused(reason) => ui.say(&format!(
                "{port}: the board refused the setting: {}.",
                crate::envelope::reason_str(reason)
            )),
            SessionReply::NoAnswer => ui.say(&format!(
                "{port}: the board did not answer, so assume nothing was persisted."
            )),
            SessionReply::ProbeSilent => ui.say(&format!(
                "{port}: the board did not answer the capability probe, so nothing was sent. {}",
                crate::envelope::PROBE_SILENCE_HINT
            )),
            SessionReply::NotAccepted => ui.say(&format!(
                "{port}: this firmware does not announce a position. Flash the current bundle first."
            )),
        }
    }
    Ok(all_took_it)
}

/// The operator-list session (audit 2026-10-12 D4, D18): send each list
/// the command line named to every running LNode, then exit. No flashing.
/// Each frame replaces its list whole; the board acks once the list is in
/// its store, and the role takes it at the next reset.
pub fn set_pn_lists(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    wires: &[leviculum_core::envelope::PnListWire],
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --static-peer/--auth-allowed/--control-allowed \
             talk to flashed boards; a board in its bootloader has no store to hold them.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        for wire in wires {
            let name = crate::pn_list::list_name(wire.list());
            let count = wire.hashes().len();
            let reply = match open_transport(sysfs, &board.device, &board.tty)
                .and_then(|fd| send_pn_list_to(&fd, *wire))
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
                    "{port}: {name} stored ({count} entries). Takes effect at the next reset; \
                     the boot PN_LISTS line on the debug port (if00) proves what came up."
                )),
                SessionReply::Refused(reason) => ui.say(&format!(
                    "{port}: the board refused {name} — {}.",
                    crate::envelope::reason_str(reason)
                )),
                SessionReply::NoAnswer => ui.say(&format!(
                    "{port}: the board did not answer for {name}, so assume nothing was stored."
                )),
                SessionReply::ProbeSilent => ui.say(&format!(
                    "{port}: the board did not answer the capability probe, so nothing was \
                     sent. {}",
                    crate::envelope::PROBE_SILENCE_HINT
                )),
                SessionReply::NotAccepted => ui.say(&format!(
                    "{port}: this firmware speaks the envelope but cannot store operator \
                     lists. Flash the current bundle first."
                )),
            }
        }
    }
    Ok(all_took_it)
}

fn send_pn_list_to(
    fd: &crate::sys::Fd,
    wire: leviculum_core::envelope::PnListWire,
) -> io::Result<SessionReply> {
    use crate::envelope;
    envelope::probed(fd, leviculum_core::envelope::TYPE_PN_LIST, |fd| {
        envelope::send_pn_list(fd, wire)
    })
}

fn send_pn_config_to(
    fd: &crate::sys::Fd,
    wire: leviculum_core::envelope::PnConfigWire,
) -> io::Result<SessionReply> {
    use crate::envelope;
    envelope::probed(fd, leviculum_core::envelope::TYPE_PN_CONFIG, |fd| {
        envelope::send_pn_config(fd, wire)
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
            Ok(TxPowerOutcome::Unreadable(answer)) => {
                all_took_it = false;
                report_unreadable_radio(ui, port, answer);
            }
            Ok(TxPowerOutcome::Answered(reply)) => {
                all_took_it &= reply.took_it();
                match reply {
                    // `send_tx_power_to` reports an ack as `Applied`, with
                    // the power it read; one arriving here is still an ack.
                    SessionReply::Acked => ui.say(&format!(
                        "{port}: the board acknowledged the radio configuration."
                    )),
                    SessionReply::Refused(reason) => ui.say(&format!(
                        "{port}: the board refused the radio configuration — {}.",
                        crate::envelope::reason_str(reason)
                    )),
                    SessionReply::NoAnswer => ui.say(&format!(
                        "{port}: the board did not answer the radio-config frame, so it is still \
                         on whatever power it had."
                    )),
                    SessionReply::ProbeSilent => ui.say(&format!(
                        "{port}: the board did not answer the capability probe, so the power \
                         was not changed. {}",
                        crate::envelope::PROBE_SILENCE_HINT
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

/// Say why the board's radio settings could not be read, and what to do
/// about *that* board.
///
/// One sentence per case, because the three cases have nothing in common but
/// the missing report:
///
/// * **No running radio.** The board answered, promptly, and its answer is
///   that it has no radio to describe. Nothing about the link or the firmware
///   version is wrong; the modem is off for this boot. Both field boards that
///   "did not report" on 2026-09-26 were this — `REFUSE_BUSY` in 0.31 ms,
///   five times over ten seconds (`lnflash/examples/radio_query_timing.rs`) —
///   and the one line they got sent the operator to flash the bundle, which
///   would not have changed a thing. The RAK4631's media report named the
///   remedy exactly: `configured_lora=1 running_lora=0`, i.e. the stored
///   profile has the carrier and only a reset starts it.
/// * **Query refused.** The firmware does not know this frame. Here, and only
///   here, is newer firmware the answer.
/// * **Silent.** The board answered the capability probe and then said
///   nothing to a frame it had just declared it accepts. That is a fault in
///   its own right and belongs on its debug port, not in a flash.
fn report_unreadable_radio(ui: &mut dyn Ui, port: &str, answer: crate::envelope::RadioQueryAnswer) {
    use crate::envelope::RadioQueryAnswer;

    let head = format!(
        "{port}: the board did not report its current radio settings, so nothing was sent. A \
         config frame carries every parameter at once; writing one without knowing the other \
         values would set the power and move the modulation with it."
    );
    match answer {
        RadioQueryAnswer::Reported(_) => {
            debug_assert!(false, "a reported profile is not an unreadable one")
        }
        RadioQueryAnswer::NoRunningRadio(reason) => ui.say(&format!(
            "{head} The board refused the radio query — {}. It answered, so this is its state \
             and not a timeout or old firmware: no radio is running on this boot, and no power \
             can be set on a modem that was never keyed. Read the board's carriers with \
             --summarize: if LoRa is configured on and not running, a reset starts it; if it is \
             configured off, switch it on with --set-media first. Flashing changes nothing here.",
            crate::envelope::reason_str(reason)
        )),
        RadioQueryAnswer::QueryRefused(reason) => ui.say(&format!(
            "{head} The board refused the radio query itself — {}. This firmware cannot be \
             swept read-modify-write; flash the current bundle first.",
            crate::envelope::reason_str(reason)
        )),
        RadioQueryAnswer::Silent => ui.say(&format!(
            "{head} The board declared in its capability report that it accepts the radio \
             query and then answered nothing readable to it within {} ms across {} attempts. \
             That silence is a board-side fault of its own: read its debug port (if00) before \
             flashing anything.",
            crate::envelope::CONTROL_TIMING.window.as_millis(),
            crate::envelope::CONTROL_TIMING.attempts
        )),
    }
}

/// What one board did with a `--set-tx-power` session.
enum TxPowerOutcome {
    /// The board reported its settings and acked the changed ones back.
    Applied { was: i8, now: i8 },
    /// The board did not report its settings, so nothing was sent. Which of
    /// the three ways it failed to report is carried along: they want three
    /// different things done about them (see [`report_unreadable_radio`]).
    Unreadable(crate::envelope::RadioQueryAnswer),
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
        return Ok(TxPowerOutcome::Answered(SessionReply::ProbeSilent));
    };
    if !caps.accepts(TYPE_RADIO_QUERY) || !caps.accepts(TYPE_RADIO_CONFIG) {
        return Ok(TxPowerOutcome::Answered(SessionReply::NotAccepted));
    }
    let mut cfg = match envelope::query_radio_config(fd)? {
        envelope::RadioQueryAnswer::Reported(cfg) => cfg,
        unreadable => return Ok(TxPowerOutcome::Unreadable(unreadable)),
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

/// The `--set-position` / `--clear-position` session: set or clear the
/// user-set fixed position on every running LNode, no flashing, then
/// exit. `position` is `None` for the clear.
///
/// While set, the fixed position replaces the board's position sensor
/// entirely in its telemetry reports — the decided semantics: the user's
/// "this is where this node is" beats a wandering fix — and it survives
/// resets. The clear returns the board to sensor reporting, which for a
/// board without a receiver honestly means no position at all.
pub fn set_fixed_position(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    position: Option<&leviculum_core::envelope::FixedPositionWire>,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-position and --clear-position talk to flashed \
             boards; a board in its bootloader has no position to configure.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let reply = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| crate::position::send_configured(&fd, position))
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
        report_fixed_position(ui, port, position, reply);
    }
    Ok(all_took_it)
}

/// Say what the board answered to the fixed position, in the same shape
/// [`report_telemetry`] uses.
fn report_fixed_position(
    ui: &mut dyn Ui,
    port: &str,
    position: Option<&leviculum_core::envelope::FixedPositionWire>,
    reply: SessionReply,
) {
    match reply {
        SessionReply::Acked => match position {
            Some(position) => ui.say(&format!(
                "{port}: fixed position set — {}. It replaces the position sensor in every \
                 telemetry report (the board's [TELEMETRY] report line on if00 says \
                 possrc=fixed) and survives resets; --clear-position returns the board to \
                 sensor reporting.",
                crate::position::describe(position)
            )),
            None => ui.say(&format!(
                "{port}: fixed position cleared — the board reports what its position sensor \
                 says again, which on a board without a receiver is no position at all."
            )),
        },
        SessionReply::Refused(reason) if reason == leviculum_core::envelope::REFUSE_UNSUPPORTED => {
            ui.say(&format!(
                "{port}: this board's firmware carries no telemetry reporter — the position was \
                 refused, not stored. Neither retrying nor rebooting helps; only firmware that \
                 wires a reporter does."
            ))
        }
        SessionReply::Refused(reason) => ui.say(&format!(
            "{port}: the board refused the fixed position — {}.",
            crate::envelope::reason_str(reason)
        )),
        SessionReply::NoAnswer => ui.say(&format!(
            "{port}: the board did not answer the fixed-position frame, so it is still on \
             whatever position source it had."
        )),
        SessionReply::ProbeSilent => ui.say(&format!(
            "{port}: the board did not answer the capability probe, so no position was \
             sent. {}",
            crate::envelope::PROBE_SILENCE_HINT
        )),
        SessionReply::NotAccepted => ui.say(&format!(
            "{port}: this firmware speaks the envelope but does not accept the fixed-position \
             frame. Flash the current bundle first."
        )),
    }
}

/// The `--set-media` session: read, and optionally set, which carriers
/// every running LNode meshes over. No flashing, then exit.
///
/// `spec` is `None` for the read-only form (`--set-media` with no value),
/// which prints what each board is on. With a spec, each board is read
/// first and the spec applied on top: a `--set-media ble=off` must not
/// reset the LoRa carrier to a host-side default, the same
/// read-modify-write contract [`set_tx_power`] holds for the radio.
pub fn set_media(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    spec: Option<crate::media::MediaSpec>,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-media talks to flashed boards; a board in its \
             bootloader has no carriers to configure.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let outcome = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| set_media_on(&fd, spec))
        {
            Ok(outcome) => outcome,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        all_took_it &= outcome.is_ok();
        report_media(ui, port, spec.is_some(), outcome);
    }
    Ok(all_took_it)
}

/// One board's half of [`set_media`]: read what it is on, then — with a
/// spec — write the merged profile back and report what the board said
/// about it.
fn set_media_on(
    fd: &crate::sys::Fd,
    spec: Option<crate::media::MediaSpec>,
) -> std::io::Result<Result<crate::envelope::MediaState, SessionReply>> {
    let current = match crate::media::query(fd)? {
        Ok(state) => state,
        // A board that cannot report its carriers is a board whose other
        // carrier we would have to invent, so nothing is written.
        Err(reply) => return Ok(Err(reply)),
    };
    let Some(spec) = spec else {
        return Ok(Ok(current));
    };
    crate::media::send_configured(fd, spec.onto(current.configured))
}

/// Say what the board answered about its carriers.
fn report_media(
    ui: &mut dyn Ui,
    port: &str,
    was_a_set: bool,
    outcome: Result<crate::envelope::MediaState, SessionReply>,
) {
    let state = match outcome {
        Ok(state) => state,
        Err(SessionReply::Refused(reason))
            if reason == leviculum_core::envelope::REFUSE_UNSUPPORTED =>
        {
            return ui.say(&format!(
                "{port}: this board's firmware carries no media gate — the profile was refused, \
                 not stored. Neither retrying nor rebooting helps; only firmware that honours \
                 the profile does. Measurements against this board cannot claim a single medium."
            ));
        }
        Err(SessionReply::Refused(reason)) => {
            return ui.say(&format!(
                "{port}: the board refused the media profile — {}.",
                crate::envelope::reason_str(reason)
            ));
        }
        Err(SessionReply::NoAnswer) => {
            return ui.say(&format!(
                "{port}: the board did not answer about its carriers, so it is still on whatever \
                 media profile it had."
            ));
        }
        Err(SessionReply::ProbeSilent) => {
            return ui.say(&format!(
                "{port}: the board did not answer the capability probe, so its media profile \
                 was left alone. {}",
                crate::envelope::PROBE_SILENCE_HINT
            ));
        }
        Err(SessionReply::NotAccepted) => {
            return ui.say(&format!(
                "{port}: this firmware speaks the envelope but has no media profile. Flash the \
                 current bundle first."
            ));
        }
        // Not reachable through `crate::media`, whose senders only ever
        // produce a report or one of the failures above — but said rather
        // than asserted: an `unreachable!` here would turn a future
        // vocabulary change into a panic on an operator's board, and this
        // line costs nothing.
        Err(SessionReply::Acked) => {
            return ui.say(&format!(
                "{port}: the board acked the media frame instead of reporting its carriers, so \
                 what it is running is unknown. Read it back with --set-media before trusting \
                 any measurement from it."
            ));
        }
    };
    let note = crate::media::reboot_note(state);
    if was_a_set {
        ui.say(&format!(
            "{port}: media profile set — running {}, configured {}. It survives resets; the \
             board's own [MEDIA] line on if00 says the same.{note}",
            crate::media::describe(state.running),
            crate::media::describe(state.configured),
        ));
    } else {
        ui.say(&format!(
            "{port}: running {}, configured {}.{note}",
            crate::media::describe(state.running),
            crate::media::describe(state.configured),
        ));
    }
}

/// The `--set-name` / `--clear-name` session: read, and optionally set,
/// what every running LNode is called. No flashing, then exit.
///
/// `name` is `None` for the read-only form (`--set-name` with no value),
/// which prints what each board is called on both surfaces;
/// `Some(None)` is `--clear-name`, back to the derived defaults; and
/// `Some(Some(..))` sets the name.
///
/// No read-modify-write here, unlike [`set_media`]: a name is one value,
/// not a profile with a carrier the caller might not have meant to touch,
/// so there is nothing to merge and nothing a host could wrongly
/// substitute a default for.
///
/// The same name goes to every board found. That is deliberate and it is
/// also why the transcript prints the port beside each answer: naming a
/// two-board bench in one command is a mistake an operator makes once, and
/// the two identical `mesh=` lines are what tells them so.
pub fn set_name(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    name: Option<Option<leviculum_core::node_name::NodeName>>,
) -> Result<bool, Error> {
    let reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --set-name talks to flashed boards; a board in its \
             bootloader has no name to configure.",
        );
        return Ok(false);
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let fd = match open_transport(sysfs, &board.device, &board.tty) {
            Ok(fd) => fd,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        let outcome = match match name {
            None => crate::name::query(&fd),
            Some(chosen) => crate::name::send(&fd, chosen.as_ref()),
        } {
            Ok(outcome) => outcome,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        all_took_it &= outcome.is_ok();
        report_name(ui, port, name, outcome);
        report_identity(ui, port, &fd);
    }
    Ok(all_took_it)
}

/// Say what the board reported about its identity hashes, on the same
/// port the name session used.
///
/// Informational — a prober's shortcut to the hashes without a
/// debug-port reader — so nothing here touches the session's success:
/// firmware from before the query still names itself fine, and is told
/// how to get the hashes rather than handed derived ones the board never
/// confirmed.
fn report_identity(ui: &mut dyn Ui, port: &str, fd: &crate::sys::Fd) {
    use leviculum_core::envelope::REFUSE_BUSY;
    match crate::name::identity(fd) {
        Ok(Ok(report)) => ui.say(&format!(
            "{port}: {}.",
            crate::name::describe_identity(&report)
        )),
        Ok(Err(SessionReply::NotAccepted)) => ui.say(&format!(
            "{port}: this firmware does not report its identity hashes; flash the current \
             bundle to read them here, or read the [IDENTITY] line on the debug port."
        )),
        Ok(Err(SessionReply::Refused(reason))) if reason == REFUSE_BUSY => ui.say(&format!(
            "{port}: the board cannot state its identity hashes yet; run the command again in \
             a second."
        )),
        // Refused otherwise, silent, or a dead port: the name outcome
        // above already told the operator what this board is; the hash
        // line is the only thing missing.
        Ok(Err(_)) | Err(_) => ui.say(&format!(
            "{port}: the board did not answer about its identity hashes."
        )),
    }
}

/// Say what the board answered about its name.
fn report_name(
    ui: &mut dyn Ui,
    port: &str,
    asked: Option<Option<leviculum_core::node_name::NodeName>>,
    outcome: Result<leviculum_core::envelope::NodeNameState, SessionReply>,
) {
    let state = match outcome {
        Ok(state) => state,
        Err(SessionReply::Refused(reason))
            if reason == leviculum_core::envelope::REFUSE_UNSUPPORTED =>
        {
            return ui.say(&format!(
                "{port}: this board's firmware carries no node name — the name was refused, not \
                 stored. Neither retrying nor rebooting helps; only firmware that honours it \
                 does. The board stays on its derived LNode-/LN- names."
            ));
        }
        Err(SessionReply::Refused(reason)) if reason == leviculum_core::envelope::REFUSE_BUSY => {
            return ui.say(&format!(
                "{port}: the board is still coming up and cannot say what it is called yet. \
                 Nothing was written; run the command again in a second."
            ));
        }
        Err(SessionReply::Refused(reason)) => {
            return ui.say(&format!(
                "{port}: the board refused the name — {}.",
                crate::envelope::reason_str(reason)
            ));
        }
        Err(SessionReply::NoAnswer) => {
            return ui.say(&format!(
                "{port}: the board did not answer about its name, so it is still called whatever \
                 it was."
            ));
        }
        Err(SessionReply::ProbeSilent) => {
            return ui.say(&format!(
                "{port}: the board did not answer the capability probe, so its name was left \
                 alone. {}",
                crate::envelope::PROBE_SILENCE_HINT
            ));
        }
        Err(SessionReply::NotAccepted) => {
            return ui.say(&format!(
                "{port}: this firmware speaks the envelope but has no settable name. Flash the \
                 current bundle first."
            ));
        }
        // Not reachable through `crate::name`, whose senders only ever
        // produce a report or one of the failures above — but said rather
        // than asserted, like the media session's arm: an `unreachable!`
        // here would turn a future vocabulary change into a panic on an
        // operator's board.
        Err(SessionReply::Acked) => {
            return ui.say(&format!(
                "{port}: the board acked the name frame instead of reporting its names, so what \
                 it is called is unknown. Read it back with --set-name."
            ));
        }
    };
    let note = crate::name::reboot_note(&state);
    let described = crate::name::describe(&state);
    match asked {
        None => ui.say(&format!("{port}: {described}.{note}")),
        Some(None) => ui.say(&format!(
            "{port}: name cleared — {described}. The board is back to the names derived from its \
             identity.{note}"
        )),
        Some(Some(_)) => ui.say(&format!(
            "{port}: name set — {described}. It survives resets; the board's own [NAME ] line on \
             if00 says the same.{note}"
        )),
    }
}

/// The `--management-identity` / `--clear-management` session (#235): who
/// may read this board's status remotely.
///
/// `allowed == None` is the read-only form — no frame that changes the
/// board goes out, so an operator can check who is on the list without
/// touching it. `Some(&[])` is the explicit clear.
///
/// The list is decided once and goes to every board found, like the
/// telemetry target: a two-board bench is not two access-control
/// decisions.
///
/// `only` narrows the session to the one board that owns that serial port
/// (any of its CDC interfaces, by path or by a `/dev/serial/by-id` link):
/// the other boards on the bus are not opened and get no frame. Without it
/// the session talks to every running board, which is the right default
/// for setting a list on a fresh batch and the wrong one on a bench where
/// one board's list is the thing under test (the T114 kept a cell's
/// identity on 2026-11-10 and the only way to read or clear just that
/// board was a hand-written envelope script).
pub fn set_mgmt(
    catalogue: &Catalogue,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    allowed: Option<&[crate::mgmt::IdentityHash]>,
    only: Option<&Path>,
) -> Result<bool, Error> {
    let mut reachable = reachable_boards(catalogue, sysfs, ui)?;
    if reachable.is_empty() {
        ui.say(
            "No running LNode on the bus. --management-identity talks to flashed boards; a board \
             in its bootloader has no allow-list to configure.",
        );
        return Ok(false);
    }
    if let Some(port) = only {
        let tty_name = port_tty_name(port);
        let listed = reachable
            .boards
            .iter()
            .map(|b| format!("  {} ({})", b.port, b.tty.display()))
            .collect::<Vec<_>>()
            .join("\n");
        reachable.boards.retain(|b| {
            tty_name
                .as_deref()
                .is_some_and(|t| device_owns_tty(&b.device, t))
        });
        // The boards left out on purpose are not this session's failures.
        reachable.unreachable = 0;
        if reachable.boards.is_empty() {
            ui.say(&format!(
                "{}: no running LNode owns this port, so nothing was sent. Running are:\n{listed}",
                port.display()
            ));
            return Ok(false);
        }
    }
    let mut all_took_it = reachable.unreachable == 0;
    for board in &reachable.boards {
        let port = &board.port;
        let fd = match open_transport(sysfs, &board.device, &board.tty) {
            Ok(fd) => fd,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        let outcome = match match allowed {
            None => crate::mgmt::query(&fd),
            Some(list) => crate::mgmt::send(&fd, list),
        } {
            Ok(outcome) => outcome,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        all_took_it &= outcome.is_ok();
        report_mgmt(ui, port, allowed, outcome);
    }
    Ok(all_took_it)
}

/// The kernel's name for the tty a port path names (`ttyACM3`), through
/// any `/dev/serial/by-id` link. A path that does not resolve (a board
/// unplugged since) is taken by its own file name, so it can still match a
/// captured tree and otherwise matches nothing.
fn port_tty_name(port: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(port).unwrap_or_else(|_| port.to_path_buf());
    resolved
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// True when one of this device's interfaces is bound to the tty `name`.
/// Every CDC of the board counts, so the debug port (if00) a person
/// usually has in hand selects the same board as its transport (if02).
fn device_owns_tty(device: &Device, name: &str) -> bool {
    device
        .interfaces
        .iter()
        .any(|i| i.tty.as_deref() == Some(name))
}

/// Say what the board reported about who may read it.
///
/// Every refusal is named rather than folded into "failed": the difference
/// between "this firmware has no allow-list" and "the record did not reach
/// flash" decides whether the operator flashes the bundle or retries, and
/// the difference between those and silence decides whether they look at
/// the cable.
fn report_mgmt(
    ui: &mut dyn Ui,
    port: &str,
    asked: Option<&[crate::mgmt::IdentityHash]>,
    outcome: Result<leviculum_core::envelope::MgmtAllowState, SessionReply>,
) {
    let state = match outcome {
        Ok(state) => state,
        Err(SessionReply::Refused(reason))
            if reason == leviculum_core::envelope::REFUSE_UNSUPPORTED =>
        {
            return ui.say(&format!(
                "{port}: this board's firmware carries no remote-management allow-list — the \
                 list was refused, not stored. Neither retrying nor rebooting helps; only \
                 firmware that honours it does. The board answers no rnstatus -R."
            ));
        }
        Err(SessionReply::Refused(reason)) if reason == leviculum_core::envelope::REFUSE_VALUE => {
            return ui.say(&format!(
                "{port}: the board refused the list as too long — it holds at most {} \
                 identities, and nothing was stored. Nothing was truncated either: the list on \
                 the board is the one it had.",
                leviculum_core::mgmt_allow_store::MGMT_ALLOW_MAX_IDENTITIES
            ));
        }
        Err(SessionReply::Refused(reason))
            if reason == leviculum_core::envelope::REFUSE_PERSIST_PENDING =>
        {
            return ui.say(&format!(
                "{port}: the board runs the allow-list and is still writing it to flash. Wait \
                 for `persist landed` on if00, or ask again (an unchanged page answers at once), \
                 before you reset it."
            ));
        }
        Err(SessionReply::Refused(reason))
            if reason == leviculum_core::envelope::REFUSE_PERSIST =>
        {
            return ui.say(&format!(
                "{port}: the board could not write the allow-list to flash, so a reset would \
                 lose it and nothing is claimed. Retry; if it keeps failing the page is the \
                 suspect, and the board's own [TELEMETRY] persist lines on if00 say which write \
                 gave up."
            ));
        }
        Err(SessionReply::Refused(reason)) => {
            return ui.say(&format!(
                "{port}: the board refused the allow-list — {}.",
                crate::envelope::reason_str(reason)
            ));
        }
        Err(SessionReply::NoAnswer) => {
            return ui.say(&format!(
                "{port}: the board did not answer about its allow-list, so whoever was on it \
                 still is."
            ));
        }
        Err(SessionReply::ProbeSilent) => {
            return ui.say(&format!(
                "{port}: the board did not answer the capability probe, so its allow-list was \
                 left alone. {}",
                crate::envelope::PROBE_SILENCE_HINT
            ));
        }
        Err(SessionReply::NotAccepted) => {
            return ui.say(&format!(
                "{port}: this firmware speaks the envelope but has no remote-management \
                 allow-list. Flash the current bundle first."
            ));
        }
        // Not reachable through `crate::mgmt`, whose senders only ever
        // produce a report or one of the failures above — said rather than
        // asserted, like the name session's arm.
        Err(SessionReply::Acked) => {
            return ui.say(&format!(
                "{port}: the board acked the allow-list frame instead of reporting it, so who \
                 may read this board is unknown. Read it back with --management-identity."
            ));
        }
    };
    let note = crate::mgmt::reboot_note(&state);
    let described = crate::mgmt::describe(&state);
    match asked {
        None => ui.say(&format!("{port}: {described}.{note}")),
        Some([]) => ui.say(&format!(
            "{port}: allow-list cleared — {described}. The board serves remote management to \
             nobody.{note}"
        )),
        Some(_) => ui.say(&format!(
            "{port}: allow-list set — {described}. It survives resets; the board's own [MGMT] \
             line on if00 says the same.{note}"
        )),
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
        let (reply, sources) = match open_transport(sysfs, &board.device, &board.tty)
            .and_then(|fd| telemetry::send_configured_and_read_sources(&fd, &target))
        {
            Ok(answer) => answer,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                all_took_it = false;
                continue;
            }
        };
        all_took_it &= reply.took_it();
        report_telemetry(ui, port, &target, reply, sources);
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
///
/// `sources` is what the board answered to the position-source query, and
/// `None` is "it did not say" — older firmware, or a binary with no
/// reporter. A board that says it has none gets the consequence sentence:
/// the target is valid configuration and is stored, so this is honest
/// information and not a refusal, but an operator who is not told will wait
/// for reports that cannot come.
fn report_telemetry(
    ui: &mut dyn Ui,
    port: &str,
    target: &leviculum_core::envelope::TelemetryTargetWire,
    reply: SessionReply,
    sources: Option<crate::envelope::PositionSources>,
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
            if sources.is_some_and(|s| !s.any()) {
                ui.say(&format!(
                    "{port}: target stored; nothing will be sent until a position source exists \
                     — set one with --set-position. Sending the position is what switches the \
                     reports on, so this board reports [TELEMETRY] target=… \
                     state=no-position-source until it has one."
                ));
            }
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
        SessionReply::Refused(reason) if reason == leviculum_core::envelope::REFUSE_UNSUPPORTED => {
            ui.say(&format!(
                "{port}: this board's firmware carries no telemetry reporter — the target was \
                 refused, not stored. Neither retrying nor rebooting helps; only firmware that \
                 wires a reporter does."
            ))
        }
        SessionReply::Refused(reason) => ui.say(&format!(
            "{port}: the board refused the telemetry target — {}.",
            crate::envelope::reason_str(reason)
        )),
        SessionReply::NoAnswer => ui.say(&format!(
            "{port}: the board did not answer the telemetry frame, so it is still on whatever \
             target it had stored."
        )),
        SessionReply::ProbeSilent => ui.say(&format!(
            "{port}: the board did not answer the capability probe, so no telemetry target was \
             sent. {}",
            crate::envelope::PROBE_SILENCE_HINT
        )),
        SessionReply::NotAccepted => ui.say(&format!(
            "{port}: this firmware speaks the envelope but has no telemetry consumer. Flash \
             the current bundle first."
        )),
    }
}

/// What a run settled about a board before it was touched, carried to the
/// identity step in its bootloader: the boards named with `--board`, and
/// those whose level question was answered yes already
/// ([`level_before_touch`]).
#[derive(Clone, Copy)]
struct BeforeTouch<'a> {
    boards: &'a [String],
    answered: &'a [String],
    /// The board a leviculum image named while it ran ([`running_ours`]).
    running: Option<&'a str>,
}

impl<'a> BeforeTouch<'a> {
    /// A board found in its bootloader, or a dry run: no question was put.
    fn nothing_asked(boards: &'a [String], running: Option<&'a str>) -> Self {
        Self {
            boards,
            answered: &[],
            running,
        }
    }
}

/// The USB manufacturer string our firmware sets on every board
/// (`usb_manufacturer` in `leviculum-nrf/src/boards/`).
pub const LEVICULUM_MANUFACTURER: &str = "leviculum";

/// The USB vendor ID our firmware's application IDs live under (pid.codes,
/// `candidate_usb` in `lnflash/catalogue.toml`). Another vendor's ID is that
/// vendor's firmware whatever its strings say.
pub const LEVICULUM_VID: u16 = 0x1209;

/// The board a running leviculum image names, before anything is touched
/// (#476): our manufacturer string on one of our own USB IDs
/// ([`LEVICULUM_VID`]) that exactly one catalogue entry lists. That image was written there by lnflash after the
/// bootloader's identity was settled, by `--board` or by the person at the
/// shared-Board-ID question, so a re-flash need not ask again.
///
/// Evidence of which model, never of what to write: the bootloader still
/// has to publish that board's own Board-ID ([`confirm_identity_after`]),
/// and a board in its bootloader, or one running anything else, names
/// nothing here.
fn running_ours(catalogue: &Catalogue, candidate: &Candidate) -> Option<String> {
    if candidate.in_bootloader
        || candidate.device.id.vid != LEVICULUM_VID
        || candidate.device.manufacturer.as_deref() != Some(LEVICULUM_MANUFACTURER)
    {
        return None;
    }
    let claimants: Vec<&String> = catalogue
        .board
        .iter()
        .filter(|(name, board)| {
            board
                .candidate_ids(name)
                .is_ok_and(|ids| ids.contains(&candidate.device.id))
        })
        .map(|(name, _)| name)
        .collect();
    match claimants.as_slice() {
        [only] => Some(only.to_string()),
        _ => None,
    }
}

/// What the bootloader says the board is and what a write would do to it,
/// said to the user. The real run and the dry run both come through here, so
/// the plan a dry run shows is the plan a real run follows. `Ok(None)` is a
/// board skipped because `--board` does not name it.
fn inspect<'m>(
    catalogue: &'m Catalogue,
    manifest: &'m Manifest,
    ui: &mut dyn Ui,
    before: BeforeTouch,
    port: &str,
    info: &InfoUf2,
    installed: impl FnOnce(&InfoUf2) -> Installed,
) -> Result<Option<(Confirmed<'m>, Plan)>, Error> {
    let Some(confirmed) = confirm_named_running(
        catalogue,
        manifest,
        info,
        port,
        before.boards,
        before.answered,
        before.running,
        ui,
    )?
    else {
        return Ok(None);
    };
    ui.say(&format!(
        "{port}: confirmed {} — Board-ID {:?}, bootloader {}",
        confirmed.name(),
        info.board_id().unwrap_or(""),
        info.banner.as_deref().unwrap_or("unreported")
    ));

    let installed = installed(info);
    ui.say(&format!("{port}: SoftDevice {}", installed.describe()));
    if installed.disagree() {
        ui.say(&format!(
            "{port}: the bootloader's SoftDevice line and the version word in flash disagree. \
             Going by the flash, which is the SoftDevice's own statement."
        ));
    }

    let req = confirmed.flashing().softdevice_req(confirmed.name())?;
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
                    req: req
                        .as_ref()
                        .map(|r| r.as_str().to_string())
                        .unwrap_or_default(),
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
                confirmed.flashing().flash.writable_start,
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
        confirmed.flashing().flash.writable_start,
    ));

    ui.say(&format!("\n{port}: this will"));
    for line in &plan {
        ui.say(line);
    }

    Ok(Some((
        confirmed,
        Plan {
            req,
            remedy_image,
            app_image,
        },
    )))
}

/// What a write would do once the identity is settled: the SoftDevice the
/// board needs, and the images that get it there.
struct Plan {
    req: Option<VersionReq>,
    remedy_image: Option<Image>,
    app_image: Image,
}

/// The one privileged thing a dry run does: mount the bootloader drive and
/// read `INFO_UF2.TXT` and `CURRENT.UF2` off it. A seam, because whether the
/// dry run goes on depends on the uid, and a test has to walk both sides of
/// that line whatever uid runs it: the forge gate runs as root (838).
trait BootDrives {
    /// Whether a mount can be attempted at all.
    fn can_mount(&self) -> bool;
    /// What the bootloader on `block` publishes, and the SoftDevice it holds.
    fn read(&self, block: &Path) -> Result<(InfoUf2, Installed), Error>;
}

/// The drives as they are: mounted, which needs root.
struct Mounted;

impl BootDrives for Mounted {
    fn can_mount(&self) -> bool {
        crate::sys::is_root()
    }

    fn read(&self, block: &Path) -> Result<(InfoUf2, Installed), Error> {
        let drive = Drive::open(block)?;
        let info = drive.info()?;
        let installed = read_installed(&drive, &info);
        Ok((info, installed))
    }
}

/// `--dry-run` for one board: say what a real run would do, and do none of
/// it. A dry run writes nothing, so it refuses nothing (838): a question the
/// identity step would put is said as "would ask", an identity the real run
/// would refuse as "would be refused", and the board is a skip either way.
/// The real run keeps 832's rule, where a refusal fails the run.
fn dry_walk(
    catalogue: &Catalogue,
    manifest: &Manifest,
    ui: &mut dyn Ui,
    opts: &Options,
    drives: &dyn BootDrives,
    candidate: &Candidate,
) {
    let port = &candidate.device.name;
    if !candidate.in_bootloader {
        ui.say(&format!(
            "{port}: would enter the bootloader and confirm what it is there. \
             --dry-run stops here, because rebooting a board is already a change to it."
        ));
        return;
    }
    if !drives.can_mount() {
        // Reading INFO_UF2.TXT means mounting, and mounting means root. Say
        // what is missing rather than reporting the board as a problem.
        ui.say(&format!(
            "{port}: already in its bootloader. Reading INFO_UF2.TXT off it needs root, so \
             run --dry-run under sudo to see the identity and SoftDevice check too."
        ));
        return;
    }

    let mut dry = DryUi {
        inner: &mut *ui,
        asked: Vec::new(),
    };
    let walked = candidate
        .device
        .block_device()
        .ok_or_else(|| Error::from(transport::Error::NoDrive { port: port.clone() }))
        .and_then(|block| drives.read(&block))
        .and_then(|(info, installed)| {
            inspect(
                catalogue,
                manifest,
                &mut dry,
                BeforeTouch::nothing_asked(&opts.boards, None),
                port,
                &info,
                |_| installed,
            )
        });
    let asked = dry.asked;
    for question in &asked {
        ui.say(&format!("{port}: would ask: {question}"));
    }
    match walked {
        Ok(Some(_)) => ui.say(&format!("{port}: --dry-run, so nothing was written.\n")),
        Ok(None) => {}
        Err(err) => ui.say(&format!("{port}: would be refused: {err}\n")),
    }
}

/// The ui a dry run's identity step talks to. Nothing is asked of the person:
/// every question is kept so the dry run can say it would be asked, and is
/// left unanswered, which is the answer an unattended real run gets.
struct DryUi<'a> {
    inner: &'a mut dyn Ui,
    asked: Vec<String>,
}

impl Ui for DryUi<'_> {
    fn say(&mut self, line: &str) {
        self.inner.say(line);
    }

    fn confirm(&mut self, question: &str) -> io::Result<bool> {
        self.asked.push(question.trim().to_string());
        Ok(false)
    }

    fn ask(&mut self, prompt: &str) -> io::Result<Option<String>> {
        self.asked.push(prompt.trim().to_string());
        Ok(None)
    }

    fn wait_for_human(&mut self, instruction: &str) -> io::Result<()> {
        self.asked.push(instruction.trim().to_string());
        Err(io::Error::other("--dry-run waits for nobody"))
    }
}

/// The `--board` decision for one candidate, taken before anything is sent
/// to it (861): `Ok(Some(line))` is a board to leave alone with that line
/// said, `Ok(None)` one to go on with.
///
/// A board found running is judged by its USB ID, against every catalogue
/// entry whose `candidate_usb` lists it. The bootloader's identity is still
/// what licenses a write; this only decides whether to knock. When none of
/// the entries is named the board is not touched at all, and when some are
/// and some are not, only its bootloader could tell, so it is refused
/// without a touch: no bootloader lnflash knows can be sent back to its
/// application from the host (the Adafruit bootloader's serial DFU activates
/// nothing without an image, `dfu_image_activate` in `dfu_single_bank.c`,
/// and its CDC port takes no 1200-baud touch). The product string the board
/// reports is said in the line but decides nothing: the catalogue holds none
/// to compare it with.
///
/// A board already in its bootloader goes on: nothing is sent to knock it
/// there, and its own `Board-ID` or bootloader ID decides
/// ([`confirm_named`], `identify_dfu`).
fn named_before_touch(
    catalogue: &Catalogue,
    candidate: &Candidate,
    named: &[String],
) -> Result<Option<String>, Error> {
    if named.is_empty() || candidate.in_bootloader {
        return Ok(None);
    }
    let id = candidate.device.id;
    let mut claimants: Vec<String> = Vec::new();
    for (name, board) in &catalogue.board {
        if board.candidate_ids(name)?.contains(&id) {
            claimants.push(name.clone());
        }
    }
    let ours: Vec<String> = claimants
        .iter()
        .filter(|c| named.contains(c))
        .cloned()
        .collect();
    let port = &candidate.device.name;
    if ours.is_empty() {
        return Ok(Some(format!(
            "{port}: running {} [{id}], an ID the catalogue gives to {}, which is not among the \
             boards named with --board ({}). Not named, not touched: nothing was sent to it, and \
             it is still running.\n",
            candidate
                .device
                .product
                .as_deref()
                .unwrap_or("unnamed firmware"),
            claimants.join(", "),
            named.join(", ")
        )));
    }
    if ours.len() < claimants.len() {
        return Err(Error::SharedRunningId {
            port: port.clone(),
            id,
            claimants,
            named: ours,
        });
    }
    Ok(None)
}

/// The level question for one candidate, put before anything is sent to it
/// (864), as [`named_before_touch`] takes the `--board` decision there.
/// `Ok` carries the boards a yes was typed for, so the bootloader naming one
/// of them is not asked again ([`confirm_identity_after`]).
///
/// A board found running is judged on its USB ID, against the catalogue
/// entries that list it and that this run may write (the named ones, when
/// boards are named; [`named_before_touch`] has already refused an ID shared
/// with one not named). Every one of them at level `expected` is asked about
/// now, while a no still leaves the board running; every one `verified` asks
/// nothing. Where they differ, only the bootloader can say which board, and
/// so which level, it is: that is said, and the question comes after the
/// touch, where a no names the way back ([`Left::Touched`]). No entry shares
/// a running ID today, so that branch is for a catalogue that grows one.
///
/// A board already in its bootloader is asked after identification, as
/// before: nothing is sent to knock it there.
fn level_before_touch(
    catalogue: &Catalogue,
    candidate: &Candidate,
    named: &[String],
    ui: &mut dyn Ui,
) -> Result<Vec<String>, Error> {
    if candidate.in_bootloader {
        return Ok(Vec::new());
    }
    let id = candidate.device.id;
    let mut claimants: Vec<(&str, Level)> = Vec::new();
    for (name, board) in &catalogue.board {
        if !named.is_empty() && !named.contains(name) {
            continue;
        }
        let Some(flashing) = board.flashing() else {
            continue;
        };
        if board.candidate_ids(name)?.contains(&id) {
            claimants.push((name, flashing.level));
        }
    }
    let port = &candidate.device.name;
    let expected = claimants
        .iter()
        .filter(|(_, level)| *level == Level::Expected)
        .count();
    if expected == 0 {
        return Ok(Vec::new());
    }
    if expected < claimants.len() {
        ui.say(&format!(
            "{port}: running [{id}], an ID the catalogue gives to {}. Only the bootloader can \
             say which, and so whether this is hardware we have never run: if it is, the \
             question comes after the touch.",
            claimants
                .iter()
                .map(|(name, level)| format!("{name} ({})", level.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        return Ok(Vec::new());
    }
    let mut answered = Vec::new();
    for (name, level) in claimants {
        put_level_question(port, name, level, Left::Running, ui)?;
        answered.push(name.to_string());
    }
    Ok(answered)
}

/// What a board the bootloader says `--board` does not name comes to: the
/// skip it always was for one found in its bootloader, and a refusal for one
/// this run touched there, since it now sits in a bootloader it was not in.
/// [`named_before_touch`] keeps that second case to a running ID that lied
/// about the board, which is why it is said rather than prevented.
fn left_in_bootloader(candidate: &Candidate) -> Result<Option<Outcome>, Error> {
    if candidate.in_bootloader {
        return Ok(None);
    }
    Err(Error::TouchedNotNamed {
        port: candidate.device.name.clone(),
        hint: candidate.hint.clone(),
    })
}

/// A level refusal after identification, on a board this run touched into
/// its bootloader: said with the way back rather than as one found there.
fn touched_at_level(err: Error, candidate: &Candidate) -> Error {
    match err {
        Error::NeedsConfirmation {
            port,
            board,
            level,
            left: Left::InBootloader,
        } if !candidate.in_bootloader => Error::NeedsConfirmation {
            port,
            board,
            level,
            left: Left::Touched,
        },
        err => err,
    }
}

fn resolve(
    catalogue: &Catalogue,
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    drives: &dyn BootDrives,
    candidate: &Candidate,
) -> Result<Option<Outcome>, Error> {
    let port = candidate.device.name.clone();

    // Before anything is touched, dry run or not: a board this tool does not
    // flash by manifest is left alone, and the reason is said out loud. It is
    // on the bus because its catalogue entry makes the control commands work
    // (Codeberg #233), and a flashing session that quietly skipped it would
    // look exactly like one that failed to see it — which is the bug that
    // entry was added to fix.
    if let Err(err) = catalogue
        .board(&candidate.hint)
        .and_then(|board| board.require_flashing(&candidate.hint))
    {
        ui.say(&format!("{port}: {err}"));
        ui.say(&format!(
            "{port}: its control commands are unaffected — --announce, --set-time, --radio-* \
             and --watch all reach it.\n"
        ));
        return Ok(None);
    }

    // Then `--board`, still before anything is sent (861): a board this run
    // is not for must be left as it was found, and one found running would
    // otherwise be touched into a bootloader nothing here can send it back
    // out of. A dry run says the same and refuses nothing (838).
    match named_before_touch(catalogue, candidate, &opts.boards) {
        Ok(None) => {}
        Ok(Some(line)) => {
            ui.say(&line);
            return Ok(None);
        }
        Err(err) if opts.dry_run => {
            ui.say(&format!("{port}: would be refused: {err}\n"));
            return Ok(None);
        }
        Err(err) => return Err(err),
    }

    // And the level question, for the same reason (864): a board that
    // needs a typed yes is asked while it still runs, so a no leaves it
    // running. A dry run says the question and what no answer comes to,
    // as it does for one found in its bootloader.
    let answered = if opts.dry_run {
        let mut dry = DryUi {
            inner: &mut *ui,
            asked: Vec::new(),
        };
        let walked = level_before_touch(catalogue, candidate, &opts.boards, &mut dry);
        let asked = dry.asked;
        for question in &asked {
            ui.say(&format!("{port}: would ask: {question}"));
        }
        if let Err(err) = walked {
            ui.say(&format!("{port}: would be refused: {err}\n"));
            return Ok(None);
        }
        Vec::new()
    } else {
        level_before_touch(catalogue, candidate, &opts.boards, ui)?
    };

    // The hint chooses how to reach the board, as it chooses how to knock;
    // what may be written is still decided on the bootloader's identity.
    let serial_dfu = catalogue
        .board(&candidate.hint)?
        .require_flashing(&candidate.hint)?
        .transport
        == Transport::DfuSerial;

    if opts.dry_run {
        if serial_dfu {
            dry_walk_dfu(catalogue, manifest, sysfs, ui, opts, candidate);
        } else {
            dry_walk(catalogue, manifest, ui, opts, drives, candidate);
        }
        return Ok(None);
    }
    if serial_dfu {
        return resolve_dfu(catalogue, manifest, sysfs, ui, opts, candidate, &answered)
            .map_err(|err| touched_at_level(err, candidate));
    }

    let running = running_ours(catalogue, candidate);
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
    let Some((confirmed, plan)) = inspect(
        catalogue,
        manifest,
        ui,
        BeforeTouch {
            boards: &opts.boards,
            answered: &answered,
            running: running.as_deref(),
        },
        &port,
        &info,
        |info| read_installed(&drive, info),
    )
    .map_err(|err| touched_at_level(err, candidate))?
    else {
        return left_in_bootloader(candidate);
    };
    let Plan {
        req,
        remedy_image,
        app_image,
    } = plan;
    confirm_overwrite(ui, opts, &port, confirmed.name())?;

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
        let declined = image.blocks_below(confirmed.flashing().flash.writable_start);
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
        resolve_identity(catalogue, manifest, &info, &port, Some(confirmed.name()))?;
        let after = read_installed(&drive, &info);
        ui.say(&format!("{port}: SoftDevice now {}", after.describe()));

        // The precondition is re-checked, not carried over. A `ui.say` in a
        // stream of progress output is not a guard, and the failure it would
        // let through — our application written onto the wrong SoftDevice —
        // is the unrecoverable one in the field (#278).
        remedy_took(&after, req.as_ref(), confirmed.name())?;
    }

    let declined = app_image.blocks_below(confirmed.flashing().flash.writable_start);
    let written = drive.write_image("APP.UF2", &app_image, declined)?;
    report_write(ui, &port, "application", &written);
    drive.close()?;
    outcome.application_written = Some(written);

    after_write(sysfs, ui, opts, &bootloader, &confirmed, outcome)
}

/// The overwrite question, put only under `--ask` (#476). Starting lnflash
/// is the decision to flash what it finds: the boards were listed before
/// anything was touched, a board this run is not for was left alone by
/// `--board`, and `--dry-run` is the preview. A question every person
/// answers yes to teaches them to answer without reading, which is the one
/// habit a question about somebody's device must not breed.
fn confirm_overwrite(
    ui: &mut dyn Ui,
    opts: &Options,
    port: &str,
    board: &str,
) -> Result<(), Error> {
    if !opts.ask {
        return Ok(());
    }
    if !ui.confirm(&format!(
        "\n{port}: overwrite the firmware on this {board}?"
    ))? {
        return Err(Error::Cancelled);
    }
    Ok(())
}

/// Everything after the application went on, whichever transport took it
/// there: wait for the board to come back, read which build it runs, and
/// set what the person asked for on the port it came back with.
fn after_write(
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    bootloader: &Device,
    confirmed: &Confirmed,
    mut outcome: Outcome,
) -> Result<Option<Outcome>, Error> {
    let port = outcome.port.clone();
    let booted = verify_boot(sysfs, ui, opts, bootloader, confirmed, &port)?;
    // The radio step needs a port to talk to, so it runs whenever the
    // application came back — including on an unconfirmed banner, where the
    // board is up and the only thing missing is the proof of which build.
    if let Some(app) = &booted.app {
        outcome.radio = set_radio(sysfs, ui, opts, app, &port)?;
        outcome.telemetry = set_telemetry_on(sysfs, ui, opts, app, &port)?;
        set_mgmt_on(sysfs, ui, opts, app, &port)?;
    }
    outcome.verdict = Some(booted.verdict);
    Ok(Some(outcome))
}

/// The bootloader of a `dfu-serial` board, once its CDC port is bound, and
/// the stable path to that port.
fn dfu_bootloader(
    sysfs: &Sysfs,
    opts: &Options,
    port: &str,
    device: &Device,
) -> Result<(Device, u8, std::path::PathBuf), Error> {
    // The device directory is on the bus a moment before the kernel binds
    // the serial driver, so a bootloader found at the start of a run may
    // have no port yet.
    let ready = entry::wait_for_bootloader(
        sysfs,
        &[device.id],
        Some(device),
        opts.appear_within,
        Transport::DfuSerial,
    )?;
    let no_port = || Error::NoDfuPort {
        port: port.to_string(),
    };
    let ready = ready.ok_or_else(no_port)?;
    let (interface, tty) = ready.first_tty().ok_or_else(no_port)?;
    let tty = sysfs.stable_tty_path(tty);
    Ok((ready.clone(), interface, tty))
}

/// What the bootloader of a `dfu-serial` board is and what the transfer
/// would send, said to the user. The real run and the dry run both come
/// through here, as they do through [`inspect`] on the UF2 path.
fn inspect_dfu<'m>(
    catalogue: &'m Catalogue,
    manifest: &'m Manifest,
    ui: &mut dyn Ui,
    before: BeforeTouch,
    port: &str,
    bootloader: &Device,
    tty: &Path,
) -> Result<Option<(Confirmed<'m>, dfu::Package)>, Error> {
    let Some(confirmed) =
        identify_dfu(catalogue, manifest, bootloader.id, port, before.boards, ui)?
    else {
        return Ok(None);
    };
    ui.say(&format!(
        "{port}: confirmed {} — bootloader {} [{}], an ID the catalogue gives this board alone; \
         a serial-DFU bootloader publishes no Board-ID",
        confirmed.name(),
        bootloader.product.as_deref().unwrap_or("unnamed"),
        bootloader.id
    ));
    let package = dfu_package(
        &confirmed.payloads().app,
        &manifest.root,
        confirmed.name(),
        confirmed.flashing(),
    )?;
    ui.say(&format!("\n{port}: this will"));
    for line in dfu_plan(&confirmed, &package, &tty.display().to_string()) {
        ui.say(&line);
    }
    ask_about_level(&confirmed, port, before.answered, ui)?;
    Ok(Some((confirmed, package)))
}

/// One `dfu-serial` board: into its bootloader, identity from the
/// bootloader's USB ID, the image over its CDC port (Codeberg #452).
fn resolve_dfu(
    catalogue: &Catalogue,
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    candidate: &Candidate,
    answered: &[String],
) -> Result<Option<Outcome>, Error> {
    let port = candidate.device.name.clone();
    let entered = if candidate.in_bootloader {
        candidate.device.clone()
    } else {
        enter_bootloader(catalogue, sysfs, ui, opts, candidate)?
    };
    let (bootloader, interface, tty) = dfu_bootloader(sysfs, opts, &port, &entered)?;
    let Some((confirmed, package)) = inspect_dfu(
        catalogue,
        manifest,
        ui,
        BeforeTouch {
            boards: &opts.boards,
            answered,
            running: None,
        },
        &port,
        &bootloader,
        &tty,
    )?
    else {
        return left_in_bootloader(candidate);
    };
    confirm_overwrite(ui, opts, &port, confirmed.name())?;

    let (mut fd, _) = open_interface(
        sysfs,
        &bootloader,
        &tty,
        interface,
        "serial-DFU port",
        "nothing was sent",
    )?;
    ui.say(&format!(
        "{port}: sending {} bytes over serial DFU on {}",
        package.firmware.len(),
        tty.display()
    ));
    let params = confirmed
        .flashing()
        .dfu_params()
        .ok_or_else(|| Error::NoDfuParams {
            board: confirmed.name().to_string(),
        })?;
    let sent = dfu::send(
        &mut fd,
        &package,
        &mut |pause| std::thread::sleep(pause),
        &mut |_, _| {},
    )
    .map_err(|source| Error::DfuFailed {
        port: port.clone(),
        source,
        params,
    })?;
    drop(fd);
    let mut line = format!(
        "{port}: sent the application ({} bytes in {} data frames",
        sent.bytes, sent.data_frames
    );
    if sent.resent > 0 {
        line.push_str(&format!(
            ", {} of them sent again after a NACK or silence",
            sent.resent
        ));
    }
    line.push(')');
    ui.say(&line);
    if sent.stop_unacknowledged {
        ui.say(&format!(
            "{port}: the port went away before the stop packet was acknowledged. The bootloader \
             resets when it activates an image, so this can be success or not: the boot check \
             below decides."
        ));
    }

    let outcome = Outcome {
        port: port.clone(),
        board: confirmed.name().to_string(),
        softdevice_installed: None,
        application_written: Some(Written {
            path: tty,
            bytes: sent.bytes,
            blocks: sent.data_frames,
            declined: 0,
            reboot_error: None,
        }),
        verdict: None,
        radio: None,
        telemetry: None,
    };
    after_write(sysfs, ui, opts, &bootloader, &confirmed, outcome)
}

/// `--dry-run` for a `dfu-serial` board: the port and the package a real run
/// would use, and nothing sent (838's rule: a dry run refuses nothing).
///
/// No mount is involved, so unlike [`dry_walk`] this needs no root to reach
/// the identity step of a board already in its bootloader.
fn dry_walk_dfu(
    catalogue: &Catalogue,
    manifest: &Manifest,
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    candidate: &Candidate,
) {
    let port = &candidate.device.name;
    if !candidate.in_bootloader {
        let knock = candidate
            .device
            .first_tty()
            .map(|(_, tty)| sysfs.stable_tty_path(tty).display().to_string())
            .unwrap_or_else(|| "its port".into());
        let bootloader_ids = catalogue
            .board(&candidate.hint)
            .and_then(|board| board.bootloader_ids(&candidate.hint))
            .map(|ids| {
                ids.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        ui.say(&format!(
            "{port}: would touch {knock} at 1200 baud, wait for its bootloader on \
             {bootloader_ids} and send the application over its serial port at {} baud. \
             --dry-run stops here, because rebooting a board is already a change to it.",
            dfu::BAUD
        ));
        let package = catalogue
            .board(&candidate.hint)
            .map_err(Error::from)
            .and_then(|board| Ok(board.require_flashing(&candidate.hint)?))
            .and_then(|flashing| {
                let payloads = manifest.payloads(&candidate.hint)?;
                dfu_package(&payloads.app, &manifest.root, &candidate.hint, flashing)
            });
        match package {
            Ok(package) => ui.say(&format!(
                "{port}: if its bootloader is a {}, the package would be: {}\n",
                candidate.hint,
                package.describe()
            )),
            Err(err) => ui.say(&format!("{port}: would be refused: {err}\n")),
        }
        return;
    }

    let mut dry = DryUi {
        inner: &mut *ui,
        asked: Vec::new(),
    };
    let walked =
        dfu_bootloader(sysfs, opts, port, &candidate.device).and_then(|(bootloader, _, tty)| {
            inspect_dfu(
                catalogue,
                manifest,
                &mut dry,
                BeforeTouch::nothing_asked(&opts.boards, None),
                port,
                &bootloader,
                &tty,
            )
            .map(|found| found.map(|_| tty))
        });
    let asked = dry.asked;
    for question in &asked {
        ui.say(&format!("{port}: would ask: {question}"));
    }
    match walked {
        Ok(Some(tty)) => ui.say(&format!(
            "{port}: --dry-run, so nothing was sent to {}.\n",
            tty.display()
        )),
        Ok(None) => {}
        Err(err) => ui.say(&format!("{port}: would be refused: {err}\n")),
    }
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
    let (reply, sources) = match open_transport(sysfs, app, &tty)
        .and_then(|fd| telemetry::send_configured_and_read_sources(&fd, &target))
    {
        Ok(answer) => answer,
        Err(err) => {
            ui.say(&format!(
                "{port}: the transport port could not be used ({err})"
            ));
            (SessionReply::NoAnswer, None)
        }
    };
    report_telemetry(ui, port, &target, reply, sources);
    Ok(Some(TelemetryOutcome { target, reply }))
}

/// Ask who may read this board's status remotely, and if anybody, send the
/// allow-list (#235).
///
/// Runs after the telemetry step because it is the same shape of question
/// about the same board on the same port, and last because it is the one an
/// operator is least likely to want: the default answer is no, and a board
/// that comes up serving nobody is the state #235 requires of an unattended
/// one. Never fails the run, for the reason [`set_radio`] gives: the
/// firmware is already written and confirmed, and a board without an
/// allow-list is a board a `lnflash --management-identity` run can still
/// fix.
///
/// The outcome is reported and not carried in [`Outcome`]: nothing
/// downstream branches on it, and the board's own `[MGMT]` line on if00 is
/// the second witness.
fn set_mgmt_on(
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    app: &Device,
    port: &str,
) -> Result<(), Error> {
    let Some(allowed) = crate::mgmt::resolve(ui, &opts.management)? else {
        return Ok(());
    };

    let Some(tty) =
        entry::wait_for_interface_tty(sysfs, app, radio::TRANSPORT_INTERFACE, opts.appear_within)?
    else {
        ui.say(&format!(
            "{port}: the firmware is on the board, but its transport port (if{:02}) never \
             appeared, so the management allow-list was not sent. Re-run `lnflash \
             --management-identity` once it enumerates.",
            radio::TRANSPORT_INTERFACE
        ));
        return Ok(());
    };

    ui.say(&format!(
        "{port}: sending the management allow-list to {} — {} identities",
        tty.display(),
        allowed.len()
    ));
    let outcome =
        match open_transport(sysfs, app, &tty).and_then(|fd| crate::mgmt::send(&fd, &allowed)) {
            Ok(outcome) => outcome,
            Err(err) => {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                return Ok(());
            }
        };
    report_mgmt(ui, port, Some(&allowed), outcome);
    Ok(())
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
    if let RadioPlan::Estimate(estimate) = &opts.radio {
        return set_estimated_radio(sysfs, ui, opts, app, port, estimate);
    }
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
            taken: radio::ConfigTaken::No,
        }));
    };

    ui.say(&format!(
        "{port}: sending radio settings to {} — {}",
        tty.display(),
        settings.describe()
    ));
    let taken = match open_transport(sysfs, app, &tty)
        .and_then(|fd| radio::send_configured(&fd, &settings))
    {
        Ok(taken) => taken,
        Err(err) => {
            ui.say(&format!(
                "{port}: the transport port could not be used ({err})"
            ));
            radio::ConfigTaken::No
        }
    };
    report_radio_taken(ui, port, &settings, taken);
    Ok(Some(RadioOutcome { settings, taken }))
}

/// What the board did with the radio settings it was sent, in words.
fn report_radio_taken(
    ui: &mut dyn Ui,
    port: &str,
    settings: &RadioSettings,
    taken: radio::ConfigTaken,
) {
    match taken {
        radio::ConfigTaken::Running => ui.say(&format!(
            "{port}: radio settings written and persisted: {}",
            settings.describe()
        )),
        // The board took the settings and said it is not on them: its boot
        // brought no LoRa carrier up, so there is no radio to program until
        // the next reset (Codeberg #363). Worth saying rather than folding
        // into success, because the operator is about to look for this node
        // on the air and it will not be there yet.
        radio::ConfigTaken::StoredForNextBoot => ui.say(&format!(
            "{port}: radio settings written and persisted: {}. The board booted with its LoRa \
             carrier switched off (--media), so nothing is on the air yet — it comes up on \
             these settings at the next reset.",
            settings.describe()
        )),
        radio::ConfigTaken::No => ui.say(&format!(
            "{port}: the firmware flashed fine, but the board did not acknowledge the radio \
             settings, so it is still on whatever it had stored — the compiled default on a \
             board that never had any. Nothing is broken: re-run lnflash, or set them from the \
             host that binds the board."
        )),
    }
}

/// Whether a profile the board reports is on the PHY of `settings`: the
/// four values that decide who hears it. Power and the airtime locks are
/// not part of being on a channel.
fn same_phy(running: &leviculum_core::rnode::RadioConfigWire, settings: &RadioSettings) -> bool {
    running.frequency_hz == settings.frequency_hz
        && running.bandwidth_hz == settings.bandwidth_hz
        && running.sf == settings.sf
        && running.cr == settings.cr
}

/// The zero-config radio step (#476): nothing asked, one rule.
///
/// 1. The board runs a profile other than the shipped one: somebody stored
///    it, the store survived the flash, and it is kept.
/// 2. It runs the shipped profile, or the estimate already: the estimate is
///    sent when it differs, and nothing when it does not.
/// 3. Which profile it runs cannot be read: nothing is sent, because a
///    config frame carries every field and inventing the ones not read is
///    how a stored profile is lost.
///
/// The estimate is [`crate::region::estimate`] over the host's place.
fn set_estimated_radio(
    sysfs: &Sysfs,
    ui: &mut dyn Ui,
    opts: &Options,
    app: &Device,
    port: &str,
    estimate: &crate::region::Estimate,
) -> Result<Option<RadioOutcome>, Error> {
    let Some(tty) =
        entry::wait_for_interface_tty(sysfs, app, radio::TRANSPORT_INTERFACE, opts.appear_within)?
    else {
        ui.say(&format!(
            "{port}: the firmware is on the board, but its transport port (if{:02}) never \
             appeared, so its radio settings were neither read nor sent. It runs what it had \
             stored, the shipped eu868 profile on a board that never had any.",
            radio::TRANSPORT_INTERFACE
        ));
        return Ok(None);
    };
    let fd = match open_transport(sysfs, app, &tty) {
        Ok(fd) => fd,
        Err(err) => {
            ui.say(&format!(
                "{port}: the transport port could not be used ({err}), so the radio settings \
                 were neither read nor sent."
            ));
            return Ok(None);
        }
    };
    let shipped = radio::EU868_PRESET.settings;
    let wanted = estimate.preset.settings;
    match crate::envelope::query_radio_config(&fd) {
        Ok(crate::envelope::RadioQueryAnswer::Reported(running))
            if !same_phy(&running, &shipped) =>
        {
            ui.say(&format!(
                "{port}: the board runs stored radio settings, not the shipped profile \
                 (freq={} bw={} sf={} cr=4/{}): kept. --radio-preset or --radio-* change them.",
                running.frequency_hz, running.bandwidth_hz, running.sf, running.cr
            ));
            Ok(None)
        }
        Ok(crate::envelope::RadioQueryAnswer::Reported(running)) if same_phy(&running, &wanted) => {
            ui.say(&format!(
                "{port}: radio on {} already ({}), nothing to send.",
                estimate.preset.name, estimate.why
            ));
            Ok(None)
        }
        Ok(crate::envelope::RadioQueryAnswer::Reported(_)) => {
            ui.say(&format!(
                "{port}: radio set to {} from the {}: {}",
                estimate.preset.name,
                estimate.why,
                wanted.describe()
            ));
            if let Some(caveat) = estimate.preset.caveat {
                ui.say(caveat);
            }
            let taken = radio::send_configured(&fd, &wanted).unwrap_or_else(|err| {
                ui.say(&format!(
                    "{port}: the transport port could not be used ({err})"
                ));
                radio::ConfigTaken::No
            });
            report_radio_taken(ui, port, &wanted, taken);
            Ok(Some(RadioOutcome {
                settings: wanted,
                taken,
            }))
        }
        Ok(other) => {
            use crate::envelope::{reason_str, RadioQueryAnswer};
            let why = match other {
                RadioQueryAnswer::NoRunningRadio(reason) => {
                    format!("no radio runs on this boot: {}", reason_str(reason))
                }
                RadioQueryAnswer::QueryRefused(reason) => {
                    format!("the board refused the radio query: {}", reason_str(reason))
                }
                RadioQueryAnswer::Silent | RadioQueryAnswer::Reported(_) => {
                    "the board did not answer the radio query".to_string()
                }
            };
            ui.say(&format!(
                "{port}: which radio profile the board runs could not be read ({why}), so \
                 nothing was sent and it keeps what it had stored. --radio-preset sets one."
            ));
            Ok(None)
        }
        Err(err) => {
            ui.say(&format!(
                "{port}: the radio profile could not be read ({err}), so nothing was sent and \
                 it keeps what it had stored."
            ));
            Ok(None)
        }
    }
}

/// Turn the plan into a choice, and say what the chosen preset obliges the
/// user to read (the us915 FCC note) — here, so the flag path and the menu
/// path cannot diverge on whether the note is printed. `None` is `Skip`.
fn resolve_radio_choice(ui: &mut dyn Ui, opts: &Options) -> Result<Option<RadioChoice>, Error> {
    let choice = match &opts.radio {
        RadioPlan::Skip => return Ok(None),
        // `set_radio` takes the estimate before it gets here; a caller that
        // did not still gets the estimated preset, never a question.
        RadioPlan::Estimate(estimate) => RadioChoice::Preset(estimate.preset),
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
    radio::EU868_PRESET
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
    // Knocking a board into its bootloader is already a change to it, and on
    // a control-only board it is a change with no purpose: nothing may be
    // written afterwards. `resolve` refuses such a board before it gets
    // here; this is the second lock on the same door, because the cost of it
    // being missed once is a board sitting in DFU with no image for it.
    let flashing = board.require_flashing(&candidate.hint)?;
    let port = candidate.device.name.clone();

    for mechanism in &flashing.entry {
        match mechanism {
            manifest::Entry::Touch1200 => {
                // Through the sysfs view's device root and by-id, like every
                // other open here: a bare `/dev/ttyACMn` is a slot number, and
                // a test bus must never reach a real port on the host.
                let Some(tty) = candidate
                    .device
                    .interface(0)
                    .and_then(|i| i.tty.as_deref())
                    .map(|name| sysfs.stable_tty_path(name))
                else {
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
                    &flashing.double_tap,
                    flashing.transport,
                ))?;
            }
        }
        let ids = flashing.bootloader_ids(&candidate.hint)?;
        if let Some(found) = entry::wait_for_bootloader(
            sysfs,
            &ids,
            Some(&candidate.device),
            opts.appear_within,
            flashing.transport,
        )? {
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
    // Only the UF2 path installs a SoftDevice, so only it comes back here.
    if let Some(found) = entry::wait_for_bootloader(
        sysfs,
        &ids,
        Some(was),
        opts.appear_within,
        Transport::Uf2Msc,
    )? {
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
    //
    // The answer is acted on rather than discarded: a board still sitting
    // on the bus in its bootloader never rebooted into what was written, so
    // whatever a port says next is about the session that was already
    // running. Reading it would answer a question about the new image with
    // a sentence from before it existed (#378).
    if !entry::wait_until_gone(sysfs, was, opts.appear_within)? {
        ui.say(&format!(
            "{port}: the bootloader is still on the bus, so the board never rebooted into what \
             was written."
        ));
        return Ok(Booted {
            verdict: Verdict::Absent,
            app: None,
        });
    }
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

    // The sha the read is directed at, and the sha the verdict is judged
    // against: the same one, because a read that does not know what it is
    // looking for has to conclude on the first banner it sees, and on
    // 2026-09-27 that was the previous firmware's, replayed (#372).
    let expected = confirmed.payloads().app.git_sha.as_deref();
    let verdict = verify::judge(running_build(sysfs, &app, expected, opts), expected);
    match &verdict {
        Verdict::Confirmed { git_sha, source } => ui.say(&format!(
            "{port}: running git_sha={git_sha}, {}. Done.",
            source.describe()
        )),
        Verdict::WrongBuild {
            saw,
            expected,
            source,
        } => ui.say(&format!(
            "{port}: the board reports git_sha={saw}, not {expected}, {}. The write did not take.",
            source.describe()
        )),
        Verdict::Unconfirmed { why } => ui.say(&format!(
            "{port}: re-enumerated, but which build it is running is unknown — {why}."
        )),
        Verdict::Absent => {}
    }
    Ok(Booted {
        verdict,
        app: Some(app),
    })
}

/// How long to wait before opening the debug port again after it went
/// away mid-read. Long enough that a board in the middle of re-enumerating
/// is not asked once per millisecond, short enough that the retry costs a
/// small fraction of the budget it spends.
const REOPEN_PAUSE: Duration = Duration::from_millis(200);

/// What the board is running **now**, or the sentence explaining why that
/// could not be established.
///
/// Three things this does that reading a window off `/dev/ttyACMn` did not
/// (Codeberg #378):
///
/// * It waits for the debug interface to bind a driver and takes the
///   **stable by-id path** ([`entry::wait_for_interface_tty`]). A board
///   that has just re-enumerated can be on the bus before its ports are,
///   and a bare tty number is a position rather than an identity — the
///   number resolved a moment ago can name a different board by the time
///   it is opened (#334 family). That other board is the one still running
///   the firmware this flash replaced, so a read that lands on it comes
///   back with exactly the pre-flash sha #378 reports.
/// * It opens through [`open_debug`], which proves after the open that the
///   fd is bound to *this* board before a byte is read.
/// * It asks [`verify::fresh_banner`] for a line emitted after the reset,
///   not for the last line in a window, and tells it which build it is
///   waiting for: a banner naming anything else is kept as evidence while
///   the read goes on, because the board repeats its banner and a stale
///   line does not (#372).
///
/// The budget covers the whole attempt, reopens included: a port that
/// vanishes mid-read (a board that resets once more on its way up) is
/// retried rather than reported, for as long as the budget lasts. A
/// non-matching banner heard on one attempt survives into the next, so a
/// board that says the old sha and then loses its port is still reported as
/// having said it.
fn running_build(
    sysfs: &Sysfs,
    app: &Device,
    expected: Option<&str>,
    opts: &Options,
) -> Result<verify::Reading, String> {
    let deadline = Instant::now() + opts.banner_budget;
    let mut why = format!(
        "no [FW_BUILD] line arrived on the debug port (if{:02}) in the {} s after the reset",
        crate::watch::DEBUG_INTERFACE,
        opts.banner_budget.as_secs()
    );
    // The best a previous attempt could show: a banner that named some other
    // build. Not an answer while there is budget left to hear the expected
    // one, and the answer once there is not.
    let mut other: Option<verify::Reading> = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return other.ok_or(why);
        }
        match entry::wait_for_interface_tty(sysfs, app, crate::watch::DEBUG_INTERFACE, remaining) {
            Ok(Some(tty)) => match open_debug_proven(sysfs, app, &tty)
                .and_then(|(fd, node)| Ok((verify::fresh_banner(&fd, expected, deadline)?, node)))
            {
                // The delay a claim carries is measured from the flush by
                // the read itself, so it says whether the line was already
                // in flight (#378) even when several lines were read.
                Ok((heard, node)) => {
                    let reading = |banner: verify::Banner| verify::Reading {
                        source: verify::Source {
                            port: tty.clone(),
                            node,
                            after: banner.after,
                            board_uptime_ms: banner.uptime_ms,
                        },
                        build: banner.build,
                    };
                    match heard {
                        verify::Heard::Expected(banner) => return Ok(reading(banner)),
                        // With no expected sha there is nothing left to wait
                        // for, so the one banner heard is the answer; with
                        // one, this is a best-so-far that a later attempt
                        // may still improve on.
                        verify::Heard::Other(banner) if expected.is_none() => {
                            return Ok(reading(banner));
                        }
                        verify::Heard::Other(banner) => other = Some(reading(banner)),
                        verify::Heard::Silence => {}
                    }
                }
                Err(err) => why = format!("the debug port could not be read ({err})"),
            },
            Ok(None) => {
                why = format!(
                    "the debug port (if{:02}) never appeared",
                    crate::watch::DEBUG_INTERFACE
                )
            }
            Err(err) => why = format!("the debug port could not be resolved ({err})"),
        }
        if Instant::now() >= deadline {
            return other.ok_or(why);
        }
        std::thread::sleep(REOPEN_PAUSE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infouf2;
    use crate::manifest::load;
    use crate::ui::testing::Fake;
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

    fn lnode_with(ttys: &[(u8, &str)]) -> Device {
        Device {
            name: "3-2.4".into(),
            id: UsbId::new(0x1209, 0x0001),
            serial: Some("DEC9947DAD9D2869".into()),
            manufacturer: None,
            product: None,
            interfaces: ttys
                .iter()
                .map(|(number, tty)| crate::usb::Interface {
                    number: *number,
                    class: 2,
                    tty: Some((*tty).to_string()),
                    block: None,
                })
                .collect(),
        }
    }

    /// `--port` with the management flags selects the board by any of its
    /// CDC ttys: the debug port a person has in hand and the transport the
    /// frame goes out on name the same board, and a neighbour's tty names
    /// none of them.
    #[test]
    fn a_management_port_selects_the_board_by_any_of_its_ttys() {
        let board = lnode_with(&[(0, "ttyACM3"), (2, "ttyACM4")]);
        assert!(device_owns_tty(&board, "ttyACM3"));
        assert!(device_owns_tty(&board, "ttyACM4"));
        assert!(!device_owns_tty(&board, "ttyACM5"));
        assert!(!device_owns_tty(&lnode_with(&[]), "ttyACM3"));
    }

    /// A by-id link resolves to the tty it points at; a path that no longer
    /// exists is read by its own file name rather than refused, so the
    /// session can still say "no running LNode owns this port".
    #[test]
    fn a_management_port_path_resolves_through_its_link() {
        let dir = TempDir::new().unwrap();
        let tty = dir.path().join("ttyACM7");
        fs::write(&tty, b"").unwrap();
        let link = dir.path().join("usb-leviculum_T114_DEC9947-if02");
        std::os::unix::fs::symlink(&tty, &link).unwrap();
        assert_eq!(port_tty_name(&link).as_deref(), Some("ttyACM7"));
        assert_eq!(
            port_tty_name(Path::new("/nonexistent/ttyACM9")).as_deref(),
            Some("ttyACM9")
        );
    }

    /// The reporter-less refusal is rendered as its own sentence, not as
    /// the generic reason string: the operator must learn that neither
    /// retrying nor rebooting helps, only different firmware does.
    #[test]
    fn a_reporterless_refusal_names_the_missing_reporter() {
        use leviculum_core::envelope::{TelemetryTargetWire, TELEMETRY_PROFILE_STATION};
        let mut ui = crate::ui::testing::Fake::agreeing();
        let target = TelemetryTargetWire {
            profile: TELEMETRY_PROFILE_STATION,
            dest_hash: [0xA7; 16],
            public_key: None,
        };
        report_telemetry(
            &mut ui,
            "ttyACM9",
            &target,
            SessionReply::Refused(leviculum_core::envelope::REFUSE_UNSUPPORTED),
            None,
        );
        let said = ui.transcript();
        assert!(said.contains("carries no telemetry reporter"), "{said}");
        assert!(said.contains("ttyACM9"), "{said}");
    }

    /// **The line the field run needed.** The three ways a radio query can
    /// fail to produce a profile are rendered as three different sentences
    /// with three different remedies, and the busy one — both field boards on
    /// 2026-09-26 — is the one that must NOT say "flash".
    ///
    /// Asserted on the remedies rather than on the phrasing: what went wrong
    /// with the old single line was not its wording but that an operator
    /// reading it flashed a board whose LoRa task only needed a reset.
    #[test]
    fn an_unreadable_radio_names_which_of_the_three_it_was() {
        use crate::envelope::RadioQueryAnswer;
        use leviculum_core::envelope::{REFUSE_BUSY, REFUSE_UNKNOWN_TYPE};

        let busy = {
            let mut ui = crate::ui::testing::Fake::agreeing();
            report_unreadable_radio(
                &mut ui,
                "ttyACM9",
                RadioQueryAnswer::NoRunningRadio(REFUSE_BUSY),
            );
            ui.transcript()
        };
        assert!(busy.contains("ttyACM9"), "{busy}");
        assert!(
            busy.contains("reset"),
            "the remedy for a dead LoRa task: {busy}"
        );
        assert!(
            busy.contains("Flashing changes nothing"),
            "the action the old line sent the operator to do: {busy}"
        );
        assert!(
            !busy.contains("timeout") || busy.contains("not a timeout"),
            "a refusal in 0.3 ms must not read as a timeout: {busy}"
        );

        let old = {
            let mut ui = crate::ui::testing::Fake::agreeing();
            report_unreadable_radio(
                &mut ui,
                "ttyACM9",
                RadioQueryAnswer::QueryRefused(REFUSE_UNKNOWN_TYPE),
            );
            ui.transcript()
        };
        assert!(
            old.contains("flash the current bundle"),
            "old firmware is the one case flashing fixes: {old}"
        );
        assert!(
            !old.contains("reset"),
            "a reset does not teach a board a frame: {old}"
        );

        let silent = {
            let mut ui = crate::ui::testing::Fake::agreeing();
            report_unreadable_radio(&mut ui, "ttyACM9", RadioQueryAnswer::Silent);
            ui.transcript()
        };
        assert!(
            silent.contains("if00"),
            "silence from a board that accepts the frame belongs on its debug port: {silent}"
        );

        // And the three are actually distinguishable, which is the property
        // the single old line did not have.
        assert_ne!(busy, old);
        assert_ne!(busy, silent);
        assert_ne!(old, silent);
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
            &mut Fake::typing(&["t114"]),
        )
        .unwrap();
        assert_eq!(confirmed.name(), "t114");
        assert_eq!(confirmed.flashing().flash.app_base, 0x2_7000);
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
            &mut Fake::default(),
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
            &mut Fake::default(),
        )
        .unwrap();
        assert_eq!(confirmed.name(), "rak4631");
        assert_eq!(confirmed.flashing().flash.app_base, 0x2_7000);
        assert_eq!(
            confirmed.flashing().identify.msc_label.as_deref(),
            Some("RAK4631")
        );
        // ...and the T114 on the same bench still resolves to the T114 image.
        let t114 = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            Some("t114"),
            &mut Fake::default(),
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
            &mut Fake::default(),
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
            &mut Fake::default(),
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
                Some("t114"),
                &mut Fake::default()
            )
            .unwrap()
            .name(),
            "t114"
        );
    }

    /// The two-board bundle with the RAK entry at level `expected`, standing
    /// in for a carrier we have checked against its vendor reference and
    /// never run.
    fn expected_rak_bundle() -> (TempDir, Catalogue, Manifest) {
        let (dir, _, _) = two_board_bundle();
        let catalogue = Catalogue::builtin_with(
            "family A).\nlevel     = \"verified\"",
            "family A).\nlevel     = \"expected\"",
        )
        .unwrap();
        let manifest = load(dir.path(), &catalogue).unwrap();
        (dir, catalogue, manifest)
    }

    #[test]
    fn an_ht_n5262_with_no_board_named_is_refused_as_a_hint() {
        // Codeberg #262: Heltec publishes HT-n5262 for three models and only
        // one takes our image. With nobody at the prompt to name it, the run
        // stops and says which models it could be and what to pass.
        let (_dir, catalogue, manifest) = bundle();
        let mut ui = Fake::default();
        let err = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
            &mut ui,
        )
        .unwrap_err();
        assert!(matches!(err, Error::AmbiguousBoardId { .. }), "{err:?}");
        let text = format!("{err}");
        for needle in [
            "Heltec Mesh Node T114, Heltec MeshSolar, Heltec Mesh Pocket",
            "Nothing was written",
            "If this is a Heltec Mesh Node T114, pass --board t114",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from: {text}");
        }
        // The person was asked, by name, before being refused.
        assert!(ui
            .transcript()
            .contains("Type t114 if this is a Heltec Mesh Node T114"));
    }

    #[test]
    fn an_ht_n5262_under_yes_alone_is_refused() {
        // `--yes` agrees to writing, not to knowing which board this is.
        let (_dir, catalogue, manifest) = bundle();
        let err = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
            &mut crate::ui::Assumed::new(true),
        )
        .unwrap_err();
        assert!(matches!(err, Error::AmbiguousBoardId { .. }), "{err:?}");
    }

    #[test]
    fn naming_another_model_at_the_prompt_refuses_the_ht_n5262() {
        let (_dir, catalogue, manifest) = bundle();
        let err = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            None,
            &mut Fake::typing(&["Mesh Pocket"]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::AmbiguousBoardId { .. }), "{err:?}");
    }

    #[test]
    fn an_ht_n5262_under_board_t114_is_confirmed_without_a_question() {
        // Naming the board on the command line is the person saying which
        // model it is, so the hint becomes a decision and nobody is asked.
        let (_dir, catalogue, manifest) = bundle();
        let mut ui = Fake::default();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            Some("t114"),
            &mut ui,
        )
        .unwrap();
        assert_eq!(confirmed.name(), "t114");
        assert!(ui.said.is_empty(), "{}", ui.transcript());
    }

    #[test]
    fn a_verified_board_with_its_own_board_id_is_confirmed_without_a_question() {
        let (_dir, catalogue, manifest) = two_board_bundle();
        let mut ui = Fake::default();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(RAK_INFO),
            "3-2.3.4.4",
            None,
            &mut ui,
        )
        .unwrap();
        assert_eq!(confirmed.name(), "rak4631");
        assert_eq!(confirmed.flashing().level, Level::Verified);
        assert!(ui.said.is_empty(), "{}", ui.transcript());
    }

    #[test]
    fn an_expected_board_is_refused_without_an_explicit_yes() {
        let (_dir, catalogue, manifest) = expected_rak_bundle();
        let info = infouf2::parse(RAK_INFO);
        // Nobody answering, `--yes`, and `--board` naming it all refuse:
        // none of them is a person saying yes to hardware we never ran.
        for (asked, ui) in [
            (None, &mut Fake::default() as &mut dyn Ui),
            (None, &mut crate::ui::Assumed::new(true)),
            (Some("rak4631"), &mut Fake::default()),
            (None, &mut Fake::typing(&["y"])),
        ] {
            let err =
                confirm_identity(&catalogue, &manifest, &info, "3-2.3.4.4", asked, ui).unwrap_err();
            assert!(matches!(err, Error::NeedsConfirmation { .. }), "{err:?}");
            let text = format!("{err}");
            assert!(text.contains("rak4631"), "{text}");
            assert!(text.contains("level expected"), "{text}");
            assert!(text.contains("never run this hardware"), "{text}");
            assert!(text.contains("Nothing was written"), "{text}");
        }
    }

    #[test]
    fn an_expected_board_is_confirmed_after_a_yes_to_a_question_that_says_so() {
        let (_dir, catalogue, manifest) = expected_rak_bundle();
        let mut ui = Fake::typing(&["yes"]);
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(RAK_INFO),
            "3-2.3.4.4",
            None,
            &mut ui,
        )
        .unwrap();
        assert_eq!(confirmed.name(), "rak4631");
        assert!(
            ui.transcript().contains("we have never run this hardware"),
            "{}",
            ui.transcript()
        );
    }

    #[test]
    fn a_bootloader_that_publishes_no_board_id_gets_nothing_written_to_it() {
        let (_dir, catalogue, manifest) = bundle();
        let info = infouf2::parse("UF2 Bootloader 0.9.0\r\nDate: Jul  9 2024\r\n");
        assert!(matches!(
            confirm_identity(
                &catalogue,
                &manifest,
                &info,
                "3-2.4",
                None,
                &mut Fake::default()
            ),
            Err(Error::NoBoardId { .. })
        ));
        // An empty value is the same as no value: it confirms nothing.
        let blank = infouf2::parse("Board-ID:  \r\n");
        assert!(matches!(
            confirm_identity(
                &catalogue,
                &manifest,
                &blank,
                "3-2.4",
                None,
                &mut Fake::default()
            ),
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
    fn a_remedy_that_took_lets_the_application_through() {
        let after = Installed {
            from_info: Some(Version::new(7, 3, 0)),
            from_flash: Some(Version::new(7, 3, 0)),
        };
        let req = VersionReq::parse(">=7.0.1, <8.0.0").unwrap();
        assert!(remedy_took(&after, Some(&req), "t114").is_ok());
    }

    #[test]
    fn a_remedy_write_that_silently_failed_stops_the_application() {
        // The board was mounted again, re-identified, and still carries the
        // factory 6.1.1: the SD.UF2 write reported success and did nothing.
        // Before #278 this printed one line and wrote the application on top.
        let after = Installed {
            from_info: Some(Version::new(6, 1, 1)),
            from_flash: Some(Version::new(6, 1, 1)),
        };
        let req = VersionReq::parse(">=7.0.1, <8.0.0").unwrap();
        let err = remedy_took(&after, Some(&req), "t114")
            .expect_err("a remedy that did not take must refuse the application write");
        match &err {
            Error::RemedyDidNotTake { board, req, found } => {
                assert_eq!(board, "t114");
                assert_eq!(req, ">=7.0.1, <8.0.0");
                assert!(found.contains("6.1.1"), "found should name the version");
            }
            other => panic!("expected RemedyDidNotTake, got {other:?}"),
        }
        assert!(
            format!("{err}").contains("NOT written"),
            "the operator must be told the application did not go on: {err}"
        );
    }

    #[test]
    fn a_version_still_unreadable_after_the_remedy_is_not_a_pass() {
        // Unknown before the remedy takes the remedy; unknown *after* it means
        // the write cannot be shown to have taken, and guessing is the brick.
        let after = Installed {
            from_info: None,
            from_flash: None,
        };
        let req = VersionReq::parse(">=7.0.1, <8.0.0").unwrap();
        assert!(matches!(
            remedy_took(&after, Some(&req), "t114"),
            Err(Error::RemedyDidNotTake { .. })
        ));
        // A board with no stated requirement never runs a remedy, and the gate
        // stays out of its way.
        assert!(remedy_took(&after, None, "t114").is_ok());
    }

    #[test]
    fn the_remedy_image_is_prepared_from_the_hex_and_fits_the_window() {
        let (_dir, catalogue, manifest) = bundle();
        let confirmed = confirm_identity(
            &catalogue,
            &manifest,
            &infouf2::parse(T114_INFO),
            "3-2.4",
            Some("t114"),
            &mut Fake::default(),
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
            Some("t114"),
            &mut Fake::default(),
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
            Some("t114"),
            &mut Fake::default(),
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
        // 3-2.3.1 is our T114 application, 3-2.4 its bootloader, 3-2.3.4.4 our
        // RAK4631 on 1209:0002 and 3-2.3.2 our Solar Node on 1209:0003. Before
        // #261 the RAK was invisible and before #233 the Solar Node was, for
        // the same reason both times: the catalogue listed no board claiming
        // that ID, so `lnflash --set-time` addressed the T114s and silently
        // skipped whatever else was on the hub.
        assert_eq!(names, vec!["3-2.3.1", "3-2.3.2", "3-2.3.4.4", "3-2.4"]);
        assert!(!found[0].in_bootloader);
        assert!(!found[1].in_bootloader);
        assert!(!found[2].in_bootloader);
        assert!(found[3].in_bootloader);
        // And each is hinted at its own board rather than at whichever entry
        // the catalogue happens to list first.
        assert!(
            found[0].describe().contains("probably a t114"),
            "{:?}",
            found[0].describe()
        );
        assert!(
            found[1].describe().contains("probably a solarnode"),
            "{:?}",
            found[1].describe()
        );
        assert!(
            found[2].describe().contains("probably a rak4631"),
            "{:?}",
            found[2].describe()
        );
        assert!(found[3].describe().contains("probably a t114"));
    }

    #[test]
    fn the_configure_only_sessions_address_every_running_board() {
        // Codeberg #261, seen from the rig: `--set-time` walked the bus, found
        // both T114s and never spoke to the Pocket V2, because no catalogue
        // entry claimed its USB ID. Codeberg #233 is the same report about the
        // Solar Node, and it is in this list for the same reason — a board
        // that carries traffic and answers control frames was addressed by
        // nothing this tool ran.
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
                ("3-2.3.2", dev.path().join("ttyACM6")),
                ("3-2.3.4.4", dev.path().join("ttyACM4")),
            ],
            "every running board, each on its own transport port (if02)"
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
        symlink("../../ttyACM6", by_id.join("usb-leviculum_SolarNode-if02")).unwrap();

        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());
        let mut ui = crate::ui::testing::Fake::agreeing();
        let found = reachable_boards(&catalogue(), &sysfs, &mut ui).unwrap();
        let ports: Vec<PathBuf> = found.boards.iter().map(|b| b.tty.clone()).collect();
        assert_eq!(
            ports,
            vec![
                by_id.join("usb-leviculum_T114-if02"),
                by_id.join("usb-leviculum_SolarNode-if02"),
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
    fn the_build_claim_names_the_port_it_was_read_on() {
        // Codeberg #378 end to end, as far as a host without a board
        // reaches: the whole read — resolve if00 from the bus, open it,
        // prove the fd, flush, wait for a line — and the provenance the
        // verdict then prints. The board is a pty, so this is safe on the
        // host that has the rig attached.
        let pty = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[("ttyACM1", &pty.slave_path)]);
        let by_id = dev.path().join("serial/by-id");
        fs::create_dir_all(&by_id).unwrap();
        let link = by_id.join("usb-leviculum_T114_183004F712B4A7FE-if00");
        symlink("../../ttyACM1", &link).unwrap();
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());
        let t114 = sysfs
            .devices()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "3-2.3.1")
            .unwrap();

        // The line already in flight when the tool arrives: what a board
        // that was talking before the flash left in the port's queue, and
        // the pre-flash sha #378 reported as the new one. It is in the
        // queue as a fact, not as a hope — the wait below returns only
        // once the port counts all of it.
        let stale = b"[FW_BUILD] git_sha=aaaaaaaa dirty=true\r\n";
        pty.write_raw(stale);
        assert!(
            pty.wait_until_queue(Duration::from_secs(5), |queued| queued >= stale.len()),
            "the stale line never reached the port's input queue"
        );

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // The board speaks once the port has been flushed, which is
                // the queue going empty again: the reset banner arrives
                // after `fresh_banner` has thrown the old line away. Waiting
                // for the flush itself, rather than sleeping past where it
                // is expected to have happened, is what keeps the order of
                // the two writes independent of the machine's load.
                assert!(
                    pty.wait_until_queue(verify::FRESH_BANNER_BUDGET, |queued| queued == 0),
                    "the read never flushed the port's input queue"
                );
                pty.write_raw(b"[FW_BUILD] git_sha=bb7c4f64 dirty=false t=7448\r\n");
            });
            let reading = running_build(&sysfs, &t114, Some("bb7c4f64"), &Options::default())
                .expect("the board spoke");
            // The banner emitted after the flush, never the one that was
            // already in flight: that is what makes the claim the board's
            // current build rather than its previous one.
            assert_eq!(reading.build.git_sha, "bb7c4f64");
            assert_eq!(reading.build.dirty, Some(false));
            // The board's own by-id link, and the node the bus names for
            // if00 behind it: the pair that settles "was this our port?"
            // from the tool's output alone.
            assert_eq!(reading.source.port, link);
            assert_eq!(reading.source.node, dev.path().join("ttyACM1"));
            // The board's own stamp on the line, carried into the claim so
            // a false negative is diagnosable from the log (#372).
            assert_eq!(reading.source.board_uptime_ms, Some(7448));
            // Information, not an assertion. How long after the flush the
            // line arrived is a property of the machine the test runs on:
            // the lower bound that used to stand here measured 139 ms on a
            // loaded Codeberg runner and failed (Woodpecker 463,
            // 2026-09-24), while the claim it stood for — the line came
            // after the flush — is the sha asserted above.
            eprintln!(
                "BANNER_AFTER_FLUSH us={} budget_us={}",
                reading.source.after.as_micros(),
                verify::FRESH_BANNER_BUDGET.as_micros(),
            );
            // The one bound the code owes: the read is given
            // FRESH_BANNER_BUDGET and stops there, so a delay past it means
            // the reader overran its own deadline rather than that the host
            // was busy.
            assert!(
                reading.source.after < verify::FRESH_BANNER_BUDGET,
                "{:?}",
                reading.source.after
            );

            let verdict = verify::judge(Ok(reading), Some("bb7c4f64"));
            assert!(verdict.is_confirmed());
            let line = verdict.describe();
            assert!(line.contains("usb-leviculum_T114"), "{line}");
            assert!(line.contains("ttyACM1"), "{line}");
            assert!(line.contains("t=7448 ms"), "{line}");
        });
    }

    #[test]
    fn a_boot_time_replay_of_the_old_banner_does_not_fail_the_flash() {
        // Codeberg #372 end to end, as the rig hit it on 2026-09-27
        // (`/home/lew/rig-run/boot-proof-flash.log`, section `=== flash
        // 01eb398b 2026-09-27T20:37:14`): the board had come back on the
        // new image, and the first `[FW_BUILD]`-bearing line on its port
        // after the flush was the new firmware quoting the OLD firmware's
        // banner out of retained RAM. The run reported "the write did not
        // take", exited 1, and stopped the script chained on it — on a
        // board whose own capture shows `git_sha=01eb398b7` seconds later.
        let pty = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[("ttyACM1", &pty.slave_path)]);
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());
        let t114 = sysfs
            .devices()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "3-2.3.1")
            .unwrap();

        // Something in the queue before the read starts, so that the queue
        // going empty below is the read's flush and nothing else. An empty
        // queue on its own proves nothing: it is empty before anybody
        // flushed it, and a writer that ran first then wrote both lines
        // into the flush (forge pipeline 571, 2026-10-26). Not a banner, so
        // the read has nothing to make of it even if it saw it.
        let pending = b"[INFO!] [BOOT] reset reason=SREQ\r\n";
        pty.write_raw(pending);
        assert!(
            pty.wait_until_queue(Duration::from_secs(5), |queued| queued >= pending.len()),
            "the pending line never reached the port's input queue"
        );

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Both lines arrive AFTER the flush, in the order the board
                // sends them: the replay block first, the new firmware's own
                // banner once its banner task ticks.
                assert!(
                    pty.wait_until_queue(verify::FRESH_BANNER_BUDGET, |queued| queued == 0),
                    "the read never flushed the port's input queue"
                );
                pty.write_raw(
                    b"[INFO!] [PERSISTENT_LOG] [INFO!] [FW_BUILD] git_sha=abaea121f                       dirty=false t=2023783 t=11
",
                );
                std::thread::sleep(Duration::from_millis(100));
                pty.write_raw(b"[INFO!] [FW_BUILD] git_sha=01eb398b7 dirty=false t=7448
");
            });
            let reading = running_build(&sysfs, &t114, Some("01eb398b7"), &Options::default())
                .expect("the board spoke");
            assert_eq!(reading.build.git_sha, "01eb398b7");
            assert_eq!(reading.source.board_uptime_ms, Some(7448));
            assert!(verify::judge(Ok(reading), Some("01eb398b7")).is_confirmed());
        });
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
        let solar_pty = crate::sys::testpty::Pty::open();
        for pty in [&t114_pty, &rak_pty, &solar_pty] {
            crate::envelope::testing::envelope_firmware_stub(pty, crate::envelope::testing::seen());
        }
        let dev = dev_tree(&[
            ("ttyACM2", &t114_pty.slave_path),
            ("ttyACM4", &rak_pty.slave_path),
            ("ttyACM6", &solar_pty.slave_path),
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
        // Every board on the bus, the Solar Node included: a session is
        // successful only if it spoke to all of them, and #233 is exactly the
        // case where one was passed over in silence.
        for port in ["3-2.3.1", "3-2.3.2", "3-2.3.4.4"] {
            assert!(
                ui.transcript().contains(&format!("{port}: telemetry on")),
                "{}",
                ui.transcript()
            );
        }
    }

    #[test]
    fn a_storm_session_reaches_every_board_and_says_what_was_accepted() {
        // The whole host path for #384's instrument: every running board,
        // the capability probe, the storm frame, the ack, and a transcript
        // an operator can act on. The stub runs the firmware's own
        // decision function, so the numbers asserted here are the numbers
        // a board would have classified.
        let t114_pty = crate::sys::testpty::Pty::open();
        let rak_pty = crate::sys::testpty::Pty::open();
        let solar_pty = crate::sys::testpty::Pty::open();
        let t114_seen = crate::envelope::testing::seen();
        let rak_seen = crate::envelope::testing::seen();
        let solar_seen = crate::envelope::testing::seen();
        crate::envelope::testing::envelope_firmware_stub(&t114_pty, t114_seen.clone());
        crate::envelope::testing::envelope_firmware_stub(&rak_pty, rak_seen.clone());
        crate::envelope::testing::envelope_firmware_stub(&solar_pty, solar_seen.clone());
        let dev = dev_tree(&[
            ("ttyACM2", &t114_pty.slave_path),
            ("ttyACM4", &rak_pty.slave_path),
            ("ttyACM6", &solar_pty.slave_path),
        ]);
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());

        let storm = leviculum_core::envelope::StoreStormWire {
            records: 176,
            size: 304,
        };
        let mut ui = crate::ui::testing::Fake::agreeing();
        let ok = store_storm(&catalogue(), &sysfs, &mut ui, storm).unwrap();
        assert!(ok, "{}", ui.transcript());
        // Every board, the same numbers.
        for seen in [&t114_seen, &rak_seen, &solar_seen] {
            assert_eq!(
                crate::envelope::testing::store_storm_frame(seen),
                Some(storm)
            );
        }
        let said = ui.transcript();
        assert!(said.contains("storm accepted"), "{said}");
        assert!(said.contains("176 records of 304 B"), "{said}");
        // The instrument is useless without the line that reports it, so
        // the session has to name where to read the result.
        assert!(said.contains("STORE storm"), "{said}");
    }

    #[test]
    fn a_storm_with_no_running_board_is_a_failed_session_not_a_quiet_one() {
        // A bench script that asked for a storm and got silence would
        // record a measurement point that never happened.
        let empty = TempDir::new().unwrap();
        let mut ui = crate::ui::testing::Fake::agreeing();
        let ok = store_storm(
            &catalogue(),
            &Sysfs::new(empty.path()),
            &mut ui,
            leviculum_core::envelope::StoreStormWire {
                records: 10,
                size: 304,
            },
        )
        .unwrap();
        assert!(!ok, "{}", ui.transcript());
        assert!(
            ui.transcript().contains("No running LNode"),
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
        let bus = run(
            &catalogue,
            &manifest,
            &Sysfs::new(empty.path()),
            &mut ui,
            &Options::default(),
        )
        .unwrap();
        assert!(bus.is_empty());
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
        let opts = Options {
            radio: RadioPlan::Ask,
            ..Options::default()
        };
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

    /// A board that answered the position-source query with a receiver and
    /// no pin — the RAK out of the box, and the board that needs no
    /// consequence sentence.
    fn has_gnss() -> crate::envelope::PositionSources {
        crate::envelope::PositionSources {
            fixed: false,
            gnss: true,
        }
    }

    /// A board that answered with nothing at all: no receiver, no pin.
    fn has_no_position_source() -> crate::envelope::PositionSources {
        crate::envelope::PositionSources {
            fixed: false,
            gnss: false,
        }
    }

    #[test]
    fn an_acked_hash_only_target_is_reported_with_what_the_node_does_next() {
        // The operator wants to know it worked. The ack says the board took
        // the frame; the node's own state line says whether it can send yet,
        // and hash-only means "not until an announce answers".
        let mut ui = crate::ui::testing::Fake::agreeing();
        report_telemetry(
            &mut ui,
            "3-2.4",
            &station_target(),
            SessionReply::Acked,
            Some(has_gnss()),
        );
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
        report_telemetry(
            &mut ui,
            "3-2.4",
            &with_key,
            SessionReply::Acked,
            Some(has_gnss()),
        );
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
            None,
        );
        let said = ui.transcript();
        assert!(said.contains("telemetry off"), "{said}");
        assert!(!said.contains("awaiting-key"), "{said}");
    }

    /// A board with no position source takes the target — it is valid
    /// configuration — and is told the consequence: nothing will be sent
    /// until it has one. An ack alone would leave the operator waiting for
    /// reports that cannot come.
    #[test]
    fn an_acked_target_on_a_board_without_a_position_source_names_the_consequence() {
        let mut ui = crate::ui::testing::Fake::agreeing();
        report_telemetry(
            &mut ui,
            "3-2.4",
            &station_target(),
            SessionReply::Acked,
            Some(has_no_position_source()),
        );
        let said = ui.transcript();
        // Honest, not a refusal: the target IS stored.
        assert!(said.contains("telemetry on"), "{said}");
        assert!(said.contains("target stored"), "{said}");
        assert!(
            said.contains("nothing will be sent until a position source exists"),
            "{said}"
        );
        // And it says what to do about it, in the flag that does it.
        assert!(said.contains("--set-position"), "{said}");
        assert!(said.contains("state=no-position-source"), "{said}");
    }

    /// **The positive control.** The same ack on a board that answered with
    /// a receiver says nothing of the kind — otherwise the sentence above
    /// would be unconditional boilerplate rather than a fact read off the
    /// board.
    #[test]
    fn control_a_board_with_a_position_source_is_not_told_it_will_stay_silent() {
        let mut ui = crate::ui::testing::Fake::agreeing();
        report_telemetry(
            &mut ui,
            "3-2.4",
            &station_target(),
            SessionReply::Acked,
            Some(has_gnss()),
        );
        let said = ui.transcript();
        assert!(said.contains("telemetry on"), "{said}");
        assert!(!said.contains("nothing will be sent"), "{said}");
        assert!(!said.contains("--set-position"), "{said}");
    }

    /// A board that did not answer the query — older firmware, or one with
    /// no reporter — is told nothing about position sources. Guessing here
    /// would put a false warning in front of an operator whose board is
    /// fine.
    #[test]
    fn a_board_that_did_not_answer_the_query_is_told_nothing_about_position_sources() {
        let mut ui = crate::ui::testing::Fake::agreeing();
        report_telemetry(
            &mut ui,
            "3-2.4",
            &station_target(),
            SessionReply::Acked,
            None,
        );
        let said = ui.transcript();
        assert!(said.contains("telemetry on"), "{said}");
        assert!(!said.contains("nothing will be sent"), "{said}");
    }

    #[test]
    fn a_board_that_cannot_take_a_target_is_told_apart_from_one_that_would_not() {
        // Three different facts, three different sentences: the operator has
        // to know whether to reflash, to retry, or to fix the value.
        for (reply, expected) in [
            (
                SessionReply::ProbeSilent,
                "did not answer the capability probe",
            ),
            (SessionReply::NotAccepted, "no telemetry consumer"),
            (SessionReply::NoAnswer, "did not answer"),
            (
                SessionReply::Refused(leviculum_core::envelope::REFUSE_VALUE),
                "refused the telemetry target",
            ),
        ] {
            let mut ui = crate::ui::testing::Fake::agreeing();
            report_telemetry(&mut ui, "3-2.4", &station_target(), reply, None);
            let said = ui.transcript();
            assert!(said.contains(expected), "{reply:?}: {said}");
            assert!(!said.contains("telemetry on"), "{reply:?}: {said}");
        }
    }

    #[test]
    fn probe_silence_names_both_of_its_causes() {
        // local-4modem-wedge: a live board whose transport port had stopped
        // being serviced answered the probe with silence, and the transcript
        // asserted "this firmware predates the control envelope" about a
        // board that had taken envelope commands the day before. Silence
        // does not identify old firmware, so the line must hand the
        // operator both readings and the reset that separates them —
        // never the age diagnosis alone.
        let mut ui = crate::ui::testing::Fake::agreeing();
        report_telemetry(
            &mut ui,
            "3-2.4",
            &station_target(),
            SessionReply::ProbeSilent,
            None,
        );
        let said = ui.transcript();
        assert!(said.contains("predates the control envelope"), "{said}");
        assert!(
            said.contains("transport port has stopped answering"),
            "{said}"
        );
        assert!(said.contains("reset it and retry"), "{said}");
    }

    #[test]
    fn the_flash_flow_asks_nothing_about_telemetry_or_management_and_sends_nothing() {
        // #476: the zero-config flash takes the question's default answer,
        // no, without putting it; `--ask` puts it again.
        let opts = Options::default();
        assert_eq!(opts.telemetry, TelemetryPlan::Skip);
        assert_eq!(opts.management, crate::mgmt::MgmtPlan::Skip);
        assert!(!opts.ask);
        let mut ui = crate::ui::testing::Fake::typing(&["yes"]);
        assert_eq!(telemetry::resolve(&mut ui, &opts.telemetry).unwrap(), None);
        assert_eq!(
            crate::mgmt::resolve(&mut ui, &opts.management).unwrap(),
            None
        );
        assert!(ui.said.is_empty(), "{}", ui.transcript());
        assert_eq!(ui.typed, ["yes"], "nothing was asked");
    }

    /// The dry run's drive reads with the uid decided by the test, not by
    /// whoever runs it: the forge gate runs as root, the coder and the land
    /// gate do not, and a verdict that depends on that is no verdict (838).
    /// Mounting is never attempted, so a root run does not go near the
    /// host's own `/dev/sdb`, which is the block device the fixture names.
    struct FakeDrives {
        root: bool,
    }

    impl BootDrives for FakeDrives {
        fn can_mount(&self) -> bool {
            self.root
        }

        fn read(&self, _block: &Path) -> Result<(InfoUf2, Installed), Error> {
            Ok((
                infouf2::parse(T114_INFO),
                Installed {
                    from_info: None,
                    from_flash: None,
                },
            ))
        }
    }

    /// A dry run over the fixture bus as `root` or not, with no `--board`.
    fn dry_run_as(root: bool) -> (Bus, Fake) {
        let (_dir, catalogue, manifest) = bundle();
        let mut ui = crate::ui::testing::Fake::agreeing();
        let opts = Options {
            dry_run: true,
            ..Options::default()
        };
        let drives = FakeDrives { root };
        let bus = run_with(&catalogue, &manifest, &sysfs(), &mut ui, &opts, &drives).unwrap();
        (bus, ui)
    }

    #[test]
    fn a_dry_run_will_not_even_reboot_a_board_into_its_bootloader() {
        // Unprivileged: the application-mode boards stop before the touch,
        // and the bootloader one (the T114 on 3-2.4) before the mount.
        let (bus, ui) = dry_run_as(false);
        // A dry run's stop is a skip, never a refusal: it must not fail.
        assert!(bus.outcomes.is_empty() && bus.refused.is_empty(), "{bus:?}");
        assert!(bus.skipped.contains(&"3-2.4".to_string()), "{bus:?}");
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED);
        let said = ui.transcript();
        assert!(said.contains("would enter the bootloader"));
        assert!(said.contains("rebooting a board is already a change"));
        assert!(said.contains("3-2.4: already in its bootloader"), "{said}");
        assert!(ui.answers.len() == 8, "nothing should have been confirmed");
    }

    #[test]
    fn a_privileged_dry_run_says_what_it_would_refuse_and_refuses_nothing() {
        // As root the dry run reads the T114's INFO_UF2.TXT and reaches the
        // identity step. HT-n5262 is shared and no --board names it, so a
        // real run would ask, go unanswered under --yes, and refuse: 832's
        // rule, which fails a real run. The dry run says so and skips (838).
        let (bus, ui) = dry_run_as(true);
        assert!(bus.outcomes.is_empty() && bus.refused.is_empty(), "{bus:?}");
        assert!(bus.skipped.contains(&"3-2.4".to_string()), "{bus:?}");
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED);
        let said = ui.transcript();
        assert!(
            said.contains("3-2.4: would ask: 3-2.4: Board-ID \"HT-n5262\""),
            "{said}"
        );
        assert!(
            said.contains("3-2.4: would be refused: the board on 3-2.4 says it is \"HT-n5262\""),
            "{said}"
        );
        assert!(said.contains("pass --board t114"), "{said}");
        assert!(!said.contains("already in its bootloader"), "{said}");
        // Nothing was put to the person: the question was kept, not asked.
        assert!(ui.answers.len() == 8, "nothing should have been confirmed");
        assert!(ui.typed.is_empty());
    }

    #[test]
    fn a_privileged_dry_run_naming_the_board_shows_the_plan_and_writes_nothing() {
        let (_dir, catalogue, manifest) = bundle();
        let mut ui = crate::ui::testing::Fake::agreeing();
        let opts = Options {
            dry_run: true,
            boards: vec!["t114".into()],
            ..Options::default()
        };
        let drives = FakeDrives { root: true };
        let bus = run_with(&catalogue, &manifest, &sysfs(), &mut ui, &opts, &drives).unwrap();
        assert!(bus.outcomes.is_empty() && bus.refused.is_empty(), "{bus:?}");
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED);
        let said = ui.transcript();
        assert!(said.contains("3-2.4: confirmed t114"), "{said}");
        assert!(said.contains("3-2.4: this will"), "{said}");
        assert!(
            said.contains("3-2.4: --dry-run, so nothing was written."),
            "{said}"
        );
        assert!(!said.contains("would ask"), "{said}");
        assert!(ui.answers.len() == 8, "nothing should have been confirmed");
    }

    /// The rig's bus for #262: a T114 and a RAK, both in their bootloaders,
    /// and what each bootloader publishes.
    fn t114_and_rak() -> Vec<(Candidate, &'static str)> {
        [
            ("3-2.4", 0x0071, "t114", T114_INFO),
            ("3-2.3.4.4", 0x0029, "rak4631", RAK_INFO),
        ]
        .into_iter()
        .map(|(port, pid, hint, info)| {
            let device = crate::usb::Device {
                name: port.to_string(),
                id: crate::usb::UsbId::new(0x239a, pid),
                serial: None,
                manufacturer: None,
                product: None,
                interfaces: Vec::new(),
            };
            let candidate = Candidate {
                device,
                in_bootloader: true,
                hint: hint.to_string(),
            };
            (candidate, info)
        })
        .collect()
    }

    /// Runs the bus loop over [`t114_and_rak`] with `--board` set to `named`,
    /// and stops each board at the identity decision: a confirmed board
    /// becomes an outcome naming it, which is the point a real run writes,
    /// and is taken as written and confirmed so the exit code is decided by
    /// the boards that were not. The ui is [`Fake::agreeing`], which answers like `--yes` (every
    /// confirm yes, every question unanswered) and keeps the transcript.
    fn flash_t114_and_rak(named: &[&str]) -> (Bus, Fake) {
        let (_dir, catalogue, manifest) = two_board_bundle();
        let bus = t114_and_rak();
        let candidates: Vec<Candidate> = bus.iter().map(|(c, _)| c.clone()).collect();
        let named: Vec<String> = named.iter().map(|n| n.to_string()).collect();
        let mut ui = Fake::agreeing();
        let result = each_candidate(&candidates, &mut ui, |ui, candidate| {
            let info = bus
                .iter()
                .find(|(c, _)| c.device.name == candidate.device.name)
                .map(|(_, info)| infouf2::parse(info))
                .unwrap();
            let port = &candidate.device.name;
            Ok(
                confirm_named(&catalogue, &manifest, &info, port, &named, &[], ui)?.map(
                    |confirmed| Outcome {
                        port: port.clone(),
                        board: confirmed.name().to_string(),
                        softdevice_installed: None,
                        application_written: Some(Written {
                            path: std::path::PathBuf::from("APP.UF2"),
                            bytes: 1_000_000,
                            blocks: 1953,
                            declined: 0,
                            reboot_error: None,
                        }),
                        verdict: Some(Verdict::Confirmed {
                            git_sha: "daa8b8e".into(),
                            source: verify::Source {
                                port: std::path::PathBuf::from("/dev/ttyACM3"),
                                node: std::path::PathBuf::from("/dev/ttyACM3"),
                                after: Duration::from_millis(2400),
                                board_uptime_ms: Some(7448),
                            },
                        }),
                        radio: None,
                        telemetry: None,
                    },
                ),
            )
        });
        (result, ui)
    }

    fn boards(bus: &Bus) -> Vec<&str> {
        bus.outcomes.iter().map(|o| o.board.as_str()).collect()
    }

    #[test]
    fn under_yes_alone_the_t114_is_refused_and_the_rak_is_still_written() {
        // 830 item 1, the rig's `lnflash --bundle B --yes` after 828: the
        // T114's refusal is said on its own line and does not abandon the
        // bus, so the RAK reaches the write. The refused T114 is not an
        // outcome; 832 keeps its port as refused, which fails the run.
        let (bus, ui) = flash_t114_and_rak(&[]);
        assert_eq!(boards(&bus), ["rak4631"]);
        assert_eq!(bus.refused, ["3-2.4"]);
        assert!(bus.skipped.is_empty(), "{:?}", bus.skipped);
        assert_eq!(bus.exit_code(), EXIT_FLASH_FAILED);
        let said = ui.transcript();
        assert!(
            said.contains("3-2.4: the board on 3-2.4 says it is \"HT-n5262\""),
            "{said}"
        );
        assert!(said.contains("pass --board t114"), "{said}");
    }

    #[test]
    fn naming_both_boards_under_yes_writes_both_without_a_question() {
        let (bus, ui) = flash_t114_and_rak(&["t114", "rak4631"]);
        assert_eq!(boards(&bus), ["t114", "rak4631"]);
        assert!(bus.refused.is_empty() && bus.skipped.is_empty(), "{bus:?}");
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED);
        assert!(ui.said.is_empty(), "{}", ui.transcript());
    }

    #[test]
    fn naming_only_the_rak_writes_it_and_skips_the_t114_with_a_line() {
        let (bus, ui) = flash_t114_and_rak(&["rak4631"]);
        assert_eq!(boards(&bus), ["rak4631"]);
        assert_eq!(bus.skipped, ["3-2.4"]);
        assert!(bus.refused.is_empty(), "{:?}", bus.refused);
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED);
        let said = ui.transcript();
        for needle in [
            "3-2.4: Board-ID \"HT-n5262\" is t114, which is not among the boards named with \
             --board (rak4631)",
            "Skipped: nothing was written",
        ] {
            assert!(said.contains(needle), "{needle:?} missing from: {said}");
        }
        // Skipped, not refused: no error line and no question.
        assert!(!said.contains("Type t114"), "{said}");
        assert!(!said.contains("asked to flash"), "{said}");
    }

    #[test]
    fn a_named_board_at_level_expected_is_still_asked_under_yes() {
        // --board says which model is present; it is not the yes to hardware
        // we never ran, and neither is --yes.
        let (_dir, catalogue, manifest) = expected_rak_bundle();
        let err = confirm_named(
            &catalogue,
            &manifest,
            &infouf2::parse(RAK_INFO),
            "3-2.3.4.4",
            &["t114".to_string(), "rak4631".to_string()],
            &[],
            &mut Fake::agreeing(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::NeedsConfirmation { .. }), "{err:?}");
    }

    // -----------------------------------------------------------------
    // dfu-serial: the T1000-E's bootloader, faked behind a pty (#452)
    // -----------------------------------------------------------------

    use crate::dfu::fake::{Bootloader, Ended, S140_7_3_0};
    use crate::dfu::Link as _;
    use std::sync::{Arc, Mutex};

    /// The application a t1000e bundle carries, as the bundle script makes
    /// it: a UF2 based at the application start. Not a multiple of 512
    /// bytes, so the last data frame is a short one.
    fn t1000e_bundle() -> (TempDir, Catalogue, Manifest) {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("t1000e")).unwrap();
        let app = Image::from_spans(
            &[crate::ihex::Span {
                start: 0x2_7000,
                data: (0..0x1300u32).map(|i| (i * 7) as u8).collect(),
            }],
            crate::uf2::FAMILY_NRF52840_APP,
        )
        .encode()
        .unwrap();
        fs::write(dir.path().join("t1000e/app.uf2"), &app).unwrap();
        let text = format!(
            "[bundle]\nversion = \"0.8.0\"\n\n[board.t1000e.app]\nfile    = \
             \"t1000e/app.uf2\"\nsha256  = \"{}\"\ngit_sha = \"bb7c4f64\"\n",
            manifest::hex_digest(&app)
        );
        fs::write(dir.path().join("manifest.toml"), text).unwrap();
        let manifest = load(dir.path(), &catalogue()).unwrap();
        (dir, catalogue(), manifest)
    }

    /// The t1000e bundle with the board back at level `expected`, where it
    /// stood until 864: the stand-in for the next board flashed over serial
    /// DFU before we have run it.
    fn expected_t1000e_bundle() -> (TempDir, Catalogue, Manifest) {
        let (dir, _, _) = t1000e_bundle();
        let catalogue = Catalogue::builtin_with(
            "what that reading gave.\nlevel     = \"verified\"",
            "what that reading gave.\nlevel     = \"expected\"",
        )
        .unwrap();
        let manifest = load(dir.path(), &catalogue).unwrap();
        (dir, catalogue, manifest)
    }

    fn sysfs_file(dir: &Path, name: &str, value: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), format!("{value}\n")).unwrap();
    }

    /// A bus with one T1000-E on it: in its bootloader (`2886:0057`, one CDC
    /// function, if00 bound to ttyACM9), or running our application with
    /// the four interfaces every leviculum board has.
    fn t1000e_bus(root: &Path, in_bootloader: bool) {
        let dev = root.join("1-1");
        sysfs_file(&dev, "serial", "DD6AB3FCACCE95DA");
        if in_bootloader {
            sysfs_file(&dev, "idVendor", "2886");
            sysfs_file(&dev, "idProduct", "0057");
            sysfs_file(&dev, "product", "T1000-E-BOOT");
            sysfs_file(&root.join("1-1:1.0"), "bInterfaceNumber", "00");
            sysfs_file(&root.join("1-1:1.0"), "bInterfaceClass", "02");
            fs::create_dir_all(root.join("1-1:1.0/tty/ttyACM9")).unwrap();
            sysfs_file(&root.join("1-1:1.1"), "bInterfaceNumber", "01");
            sysfs_file(&root.join("1-1:1.1"), "bInterfaceClass", "0a");
        } else {
            sysfs_file(&dev, "idVendor", "1209");
            sysfs_file(&dev, "idProduct", "0004");
            sysfs_file(&dev, "product", "leviculum T1000-E");
            for (n, class, tty) in [("0", "02", Some("ttyACM7")), ("2", "02", Some("ttyACM8"))] {
                let iface = root.join(format!("1-1:1.{n}"));
                sysfs_file(&iface, "bInterfaceNumber", &format!("0{n}"));
                sysfs_file(&iface, "bInterfaceClass", class);
                if let Some(tty) = tty {
                    fs::create_dir_all(iface.join("tty").join(tty)).unwrap();
                }
            }
        }
    }

    /// The board leaving its bootloader for the application it was given:
    /// the bootloader's device goes, the application's arrives on the same
    /// port with the same serial, and it has not bound a debug port yet.
    fn t1000e_reboots_into_the_application(root: &Path) {
        for iface in ["1-1:1.0", "1-1:1.1"] {
            let _ = fs::remove_dir_all(root.join(iface));
        }
        sysfs_file(&root.join("1-1"), "product", "leviculum T1000-E");
        sysfs_file(&root.join("1-1"), "idVendor", "1209");
        sysfs_file(&root.join("1-1"), "idProduct", "0004");
    }

    struct DfuBench {
        sys: TempDir,
        dev: TempDir,
        board: Arc<Mutex<Bootloader>>,
        heard: Arc<Mutex<usize>>,
    }

    impl DfuBench {
        fn sysfs(&self) -> Sysfs {
            Sysfs::with_dev(self.sys.path(), self.dev.path())
        }
    }

    /// The T1000-E in its bootloader, with `board` answering on its CDC
    /// port. The fake hangs up when the bootloader would reset, and on a
    /// good image the bus then shows the application, as the rig would.
    fn t1000e_in_bootloader(board: Bootloader) -> DfuBench {
        let sys = TempDir::new().unwrap();
        t1000e_bus(sys.path(), true);
        let pty = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[("ttyACM9", &pty.slave_path)]);
        let (board, heard) = serve_bootloader(pty, board, sys.path());
        DfuBench {
            sys,
            dev,
            board,
            heard,
        }
    }

    /// `board` answering on `pty`, the bootloader's CDC port, with the bus
    /// under `root` showing the application once it activates an image.
    #[allow(clippy::type_complexity)]
    fn serve_bootloader(
        pty: crate::sys::testpty::Pty,
        board: Bootloader,
        root: &Path,
    ) -> (Arc<Mutex<Bootloader>>, Arc<Mutex<usize>>) {
        let board = Arc::new(Mutex::new(board));
        let heard = Arc::new(Mutex::new(0usize));
        let (inside, count, root) = (board.clone(), heard.clone(), root.to_path_buf());
        crate::sys::testpty::spawn_raw(pty, move |bytes| {
            *count.lock().unwrap() += bytes.len();
            let mut board = inside.lock().unwrap();
            board.write(bytes).unwrap();
            let answer = board.read(Duration::ZERO).unwrap().unwrap_or_default();
            let gone = board.is_gone();
            if board.ended == Some(Ended::Activated) {
                t1000e_reboots_into_the_application(&root);
            }
            (answer, gone)
        });
        (board, heard)
    }

    fn dfu_opts() -> Options {
        Options {
            appear_within: Duration::from_secs(2),
            banner_budget: Duration::from_millis(300),
            radio: RadioPlan::Skip,
            management: crate::mgmt::MgmtPlan::Skip,
            ..Options::default()
        }
    }

    #[test]
    fn a_t1000e_in_its_bootloader_is_flashed_over_serial_dfu() {
        let (_dir, catalogue, manifest) = expected_t1000e_bundle();
        let bench = t1000e_in_bootloader(Bootloader::new(S140_7_3_0));
        // Yes to the overwrite, and yes to hardware at level expected.
        let mut ui = Fake::typing(&["yes"]);
        let bus = run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            &mut ui,
            &dfu_opts(),
            &FakeDrives { root: false },
        )
        .unwrap();
        let said = ui.transcript();
        assert!(bus.refused.is_empty(), "{said}");
        let outcome = &bus.outcomes[0];
        assert_eq!(outcome.board, "t1000e");
        let written = outcome.application_written.as_ref().unwrap();
        assert_eq!(written.path, bench.dev.path().join("ttyACM9"));

        let board = bench.board.lock().unwrap();
        assert_eq!(board.ended, Some(Ended::Activated), "{said}");
        // The image is the payload from 0x27000 on, to the end of its last
        // UF2 block, and the bootloader checked it against the init packet.
        assert_eq!(board.image.len(), 0x1300);
        assert_eq!(board.image[..4], [0, 7, 14, 21]);
        assert_eq!(written.bytes, 0x1300);
        assert_eq!(written.blocks, 10);

        for needle in [
            "1-1: confirmed t1000e — bootloader T1000-E-BOOT [2886:0057]",
            "level expected",
            "package: device type 0x0052, device revision 0xffff, application version \
             0xffffffff, SoftDevice requirement 0x0123, 4864 bytes in 10 data frames",
            "the start packet erases that area",
            "1-1: sent the application (4864 bytes in 10 data frames)\n",
            "1-1: back as leviculum T1000-E [1209:0004]",
        ] {
            assert!(said.contains(needle), "{needle:?} missing from:\n{said}");
        }
        // Back, and nobody could read which build: the fake has no debug
        // port. Written and not contradicted is exit 2, not a failure.
        assert_eq!(bus.exit_code(), EXIT_UNCONFIRMED, "{said}");
    }

    #[test]
    fn a_board_on_a_softdevice_the_package_does_not_name_fails_and_says_what_is_left() {
        // S140 6.1.1 (0x00B6): the bootloader acknowledges the init packet,
        // reads it, and resets. The application area is already erased.
        let (_dir, catalogue, manifest) = t1000e_bundle();
        let bench = t1000e_in_bootloader(Bootloader::new(0x00B6));
        let mut ui = Fake::typing(&["yes"]);
        let bus = run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            &mut ui,
            &dfu_opts(),
            &FakeDrives { root: false },
        )
        .unwrap();
        let said = ui.transcript();
        assert_eq!(bus.refused, vec!["1-1".to_string()], "{said}");
        assert_eq!(bus.exit_code(), EXIT_FLASH_FAILED);
        assert_eq!(
            bench.board.lock().unwrap().ended,
            Some(Ended::Refused("SoftDevice requirement"))
        );
        for needle in [
            "1-1: serial DFU did not finish: data frame 1 of 10: the port went away",
            "device type 0x0052 and a SoftDevice among 0x0123 only",
            "already erased the application area",
            "Run lnflash again; if it stops at the same place twice, the bootloader is refusing \
             the package",
        ] {
            assert!(said.contains(needle), "{needle:?} missing from:\n{said}");
        }
    }

    #[test]
    fn a_frame_the_bootloader_keeps_refusing_fails_the_flash_at_that_frame() {
        let (_dir, catalogue, manifest) = t1000e_bundle();
        let mut board = Bootloader::new(S140_7_3_0);
        board.nack_data = Some((4, usize::MAX));
        let bench = t1000e_in_bootloader(board);
        let mut ui = Fake::typing(&["yes"]);
        let bus = run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            &mut ui,
            &dfu_opts(),
            &FakeDrives { root: false },
        )
        .unwrap();
        let said = ui.transcript();
        assert_eq!(bus.exit_code(), EXIT_FLASH_FAILED, "{said}");
        assert!(
            said.contains("serial DFU did not finish: data frame 4 of 10"),
            "{said}"
        );
        assert!(said.contains("3 times in a row"), "{said}");
        assert!(
            said.contains("already erased the application area"),
            "{said}"
        );
        assert_eq!(bench.board.lock().unwrap().ended, None);
    }

    #[test]
    fn a_t1000e_is_not_written_without_a_yes_to_its_level() {
        let (_dir, catalogue, manifest) = expected_t1000e_bundle();
        let bench = t1000e_in_bootloader(Bootloader::new(S140_7_3_0));
        let mut ui = Fake::agreeing();
        let bus = run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            &mut ui,
            &dfu_opts(),
            &FakeDrives { root: false },
        )
        .unwrap();
        assert_eq!(bus.exit_code(), EXIT_FLASH_FAILED);
        let said = ui.transcript();
        assert!(said.contains("Nothing was written"), "{said}");
        assert!(
            said.contains("and it is still in its bootloader."),
            "{said}"
        );
        assert_eq!(*bench.heard.lock().unwrap(), 0, "not a byte was sent");
    }

    #[test]
    fn a_dry_run_names_the_port_and_the_package_and_sends_nothing() {
        // No root needed: nothing is mounted on this transport. The level
        // question is said as one a real run would ask (838).
        let (_dir, catalogue, manifest) = expected_t1000e_bundle();
        let bench = t1000e_in_bootloader(Bootloader::new(S140_7_3_0));
        let mut ui = Fake::agreeing();
        let opts = Options {
            dry_run: true,
            ..dfu_opts()
        };
        let bus = run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            &mut ui,
            &opts,
            &FakeDrives { root: false },
        )
        .unwrap();
        let said = ui.transcript();
        assert!(bus.refused.is_empty() && bus.outcomes.is_empty(), "{said}");
        assert_eq!(bus.skipped, vec!["1-1".to_string()]);
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED);
        let tty = bench.dev.path().join("ttyACM9");
        for needle in [
            format!("over serial DFU on {} at 115200 baud", tty.display()),
            "package: device type 0x0052, device revision 0xffff, application version \
             0xffffffff, SoftDevice requirement 0x0123, 4864 bytes in 10 data frames, image \
             CRC16 0x"
                .to_string(),
            "1-1: would be refused: the board on 1-1 is t1000e, which lnflash knows at level \
             expected"
                .to_string(),
            "1-1: would ask: 1-1: t1000e is at level expected".to_string(),
        ] {
            assert!(said.contains(&needle), "{needle:?} missing from:\n{said}");
        }
        assert_eq!(*bench.heard.lock().unwrap(), 0, "not a byte was sent");
        assert!(ui.answers.len() == 8, "nothing should have been confirmed");
    }

    #[test]
    fn a_dry_run_on_a_running_t1000e_names_the_touch_and_the_package() {
        let (_dir, catalogue, manifest) = t1000e_bundle();
        let sys = TempDir::new().unwrap();
        t1000e_bus(sys.path(), false);
        let sysfs = Sysfs::with_dev(sys.path(), "/nonexistent-dev");
        let mut ui = Fake::agreeing();
        let opts = Options {
            dry_run: true,
            ..dfu_opts()
        };
        let bus = run_with(
            &catalogue,
            &manifest,
            &sysfs,
            &mut ui,
            &opts,
            &FakeDrives { root: false },
        )
        .unwrap();
        let said = ui.transcript();
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED, "{said}");
        for needle in [
            "1-1: would touch /nonexistent-dev/ttyACM7 at 1200 baud, wait for its bootloader on \
             2886:0057 and send the application over its serial port at 115200 baud",
            "1-1: if its bootloader is a t1000e, the package would be: device type 0x0052",
        ] {
            assert!(said.contains(needle), "{needle:?} missing from:\n{said}");
        }
    }

    // -----------------------------------------------------------------
    // --board before the touch (861)
    // -----------------------------------------------------------------

    /// A T1000-E running our application, its transport port 0 a pty that
    /// shows a 1200-baud touch, and behind a second pty the bootloader it
    /// would come up in. A watcher plays the reboot: when the port is set to
    /// 1200 baud, the application's interfaces go and the bootloader's
    /// arrive on the same USB port, as the rig unit does.
    struct TouchBench {
        sys: TempDir,
        dev: TempDir,
        touched: Arc<std::sync::atomic::AtomicBool>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        board: Arc<Mutex<Bootloader>>,
        heard: Arc<Mutex<usize>>,
    }

    impl TouchBench {
        fn sysfs(&self) -> Sysfs {
            Sysfs::with_dev(self.sys.path(), self.dev.path())
        }

        fn touched(&self) -> bool {
            self.touched.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for TouchBench {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn running_t1000e() -> TouchBench {
        use std::sync::atomic::{AtomicBool, Ordering};
        let sys = TempDir::new().unwrap();
        t1000e_bus(sys.path(), false);
        let app = crate::sys::testpty::Pty::open();
        let boot = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[("ttyACM7", &app.slave_path), ("ttyACM9", &boot.slave_path)]);
        assert_ne!(app.speed(), libc::B1200, "the port starts at another speed");
        let (board, heard) = serve_bootloader(boot, Bootloader::new(S140_7_3_0), sys.path());
        let touched = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, halt, root) = (touched.clone(), stop.clone(), sys.path().to_path_buf());
        std::thread::spawn(move || {
            while !halt.load(Ordering::SeqCst) {
                if app.speed() == libc::B1200 {
                    flag.store(true, Ordering::SeqCst);
                    for iface in ["1-1:1.0", "1-1:1.2"] {
                        let _ = fs::remove_dir_all(root.join(iface));
                    }
                    t1000e_bus(&root, true);
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        TouchBench {
            sys,
            dev,
            touched,
            stop,
            board,
            heard,
        }
    }

    fn run_on(bench: &TouchBench, ui: &mut Fake, opts: &Options) -> Bus {
        run_on_bundle(bench, ui, opts, t1000e_bundle())
    }

    fn run_on_bundle(
        bench: &TouchBench,
        ui: &mut Fake,
        opts: &Options,
        (_dir, catalogue, manifest): (TempDir, Catalogue, Manifest),
    ) -> Bus {
        run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            ui,
            opts,
            &FakeDrives { root: false },
        )
        .unwrap()
    }

    const NOT_NAMED: &str = "1-1: running leviculum T1000-E [1209:0004], an ID the catalogue \
                             gives to t1000e, which is not among the boards named with --board \
                             (t114, rak4631). Not named, not touched: nothing was sent to it, \
                             and it is still running.";

    #[test]
    fn a_running_t1000e_not_named_is_left_running_and_never_touched() {
        // The rig's bank flash on 2026-10-19: --board t114 --board rak4631,
        // and a T1000-E on the hub. It was touched into a bootloader with no
        // volume and sat there 34 minutes. Now nothing is sent to it.
        let bench = running_t1000e();
        let mut ui = Fake::agreeing();
        let opts = Options {
            boards: vec!["t114".into(), "rak4631".into()],
            ..dfu_opts()
        };
        let bus = run_on(&bench, &mut ui, &opts);
        let said = ui.transcript();
        assert_eq!(bus.skipped, ["1-1"], "{said}");
        assert!(bus.refused.is_empty() && bus.outcomes.is_empty(), "{said}");
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED);
        assert!(said.contains(NOT_NAMED), "{said}");
        assert!(!said.contains("1200-baud touch"), "{said}");
        assert!(!bench.touched(), "the port was set to 1200 baud: {said}");
        assert_eq!(*bench.heard.lock().unwrap(), 0, "not a byte was sent");
    }

    #[test]
    fn a_dry_run_says_the_same_not_named_not_touched() {
        let bench = running_t1000e();
        let mut ui = Fake::agreeing();
        let opts = Options {
            dry_run: true,
            boards: vec!["t114".into(), "rak4631".into()],
            ..dfu_opts()
        };
        let bus = run_on(&bench, &mut ui, &opts);
        let said = ui.transcript();
        assert_eq!(bus.skipped, ["1-1"], "{said}");
        assert!(said.contains(NOT_NAMED), "{said}");
        assert!(!said.contains("would touch"), "{said}");
        assert!(!bench.touched());
    }

    #[test]
    fn a_running_t1000e_named_is_touched_and_flashed_over_serial_dfu() {
        let bench = running_t1000e();
        // Verified: no question to answer; the overwrite is a confirm.
        let mut ui = Fake::agreeing();
        let opts = Options {
            boards: vec!["t1000e".into()],
            ..dfu_opts()
        };
        let bus = run_on(&bench, &mut ui, &opts);
        let said = ui.transcript();
        assert!(bench.touched(), "{said}");
        assert!(bus.refused.is_empty() && bus.skipped.is_empty(), "{said}");
        assert_eq!(bus.outcomes[0].board, "t1000e");
        assert_eq!(
            bench.board.lock().unwrap().ended,
            Some(Ended::Activated),
            "{said}"
        );
        for needle in [
            format!(
                "1-1: 1200-baud touch on {}",
                bench.dev.path().join("ttyACM7").display()
            ),
            "1-1: confirmed t1000e — bootloader T1000-E-BOOT [2886:0057]".to_string(),
            "1-1: sent the application (4864 bytes in 10 data frames)".to_string(),
        ] {
            assert!(said.contains(&needle), "{needle:?} missing from:\n{said}");
        }
        assert!(!said.contains("Not named"), "{said}");
        assert!(!said.contains("level expected"), "{said}");
        assert_eq!(bus.exit_code(), EXIT_UNCONFIRMED, "{said}");
    }

    #[test]
    fn a_dry_run_naming_the_t1000e_would_touch_it_and_does_not() {
        let bench = running_t1000e();
        let mut ui = Fake::agreeing();
        let opts = Options {
            dry_run: true,
            boards: vec!["t1000e".into()],
            ..dfu_opts()
        };
        let bus = run_on(&bench, &mut ui, &opts);
        let said = ui.transcript();
        assert_eq!(bus.skipped, ["1-1"], "{said}");
        assert!(said.contains("1-1: would touch"), "{said}");
        assert!(!said.contains("Not named"), "{said}");
        assert!(!bench.touched());
    }

    #[test]
    fn a_running_id_two_entries_share_is_refused_untouched_when_only_one_is_named() {
        // No entry shares a running ID today. If one ever does, only the
        // bootloader could tell the boards apart, and a wrong guess there
        // has no way back, so it is not touched to find out.
        let catalogue = Catalogue::builtin_with(
            "candidate_usb = [\"1209:0002\"]",
            "candidate_usb = [\"1209:0002\", \"1209:0004\"]",
        )
        .unwrap();
        let sys = TempDir::new().unwrap();
        t1000e_bus(sys.path(), false);
        let candidate = find_candidates(&catalogue, &Sysfs::new(sys.path()))
            .unwrap()
            .remove(0);
        let err = named_before_touch(&catalogue, &candidate, &["t1000e".to_string()]).unwrap_err();
        assert!(matches!(err, Error::SharedRunningId { .. }), "{err:?}");
        let text = err.to_string();
        assert!(text.contains("gives to rak4631, t1000e"), "{text}");
        assert!(text.contains("only t1000e is named"), "{text}");
        assert!(text.contains("it was not touched"), "{text}");
        // Both named: no question about which, so it goes on to the touch.
        let both = ["t1000e".to_string(), "rak4631".to_string()];
        assert!(named_before_touch(&catalogue, &candidate, &both)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_board_touched_and_then_not_named_is_refused_with_the_way_back() {
        let candidate = Candidate {
            device: crate::usb::Device {
                name: "1-1".into(),
                id: crate::usb::UsbId::new(0x1209, 0x0004),
                serial: None,
                manufacturer: None,
                product: None,
                interfaces: Vec::new(),
            },
            in_bootloader: false,
            hint: "t1000e".into(),
        };
        let err = left_in_bootloader(&candidate).unwrap_err();
        assert!(err.to_string().contains("press RESET once"), "{err}");
        let found_there = Candidate {
            in_bootloader: true,
            ..candidate
        };
        assert!(left_in_bootloader(&found_there).unwrap().is_none());
    }

    // -----------------------------------------------------------------
    // the level question before the touch (864)
    // -----------------------------------------------------------------

    #[test]
    fn a_running_expected_board_without_a_yes_is_refused_and_never_touched() {
        // The rig on 2026-10-20: `lnflash --board t1000e --yes` touched the
        // board, identified it in its bootloader, and only then refused for
        // want of a typed yes. Now the question comes while it still runs.
        let bench = running_t1000e();
        for ui in [Fake::agreeing(), Fake::typing(&["y"])] {
            let mut ui = ui;
            let opts = Options {
                boards: vec!["t1000e".into()],
                ..dfu_opts()
            };
            let bus = run_on_bundle(&bench, &mut ui, &opts, expected_t1000e_bundle());
            let said = ui.transcript();
            assert_eq!(bus.refused, ["1-1"], "{said}");
            assert_eq!(bus.exit_code(), EXIT_FLASH_FAILED, "{said}");
            for needle in [
                "1-1: t1000e is at level expected",
                "Type yes to write it anyway:",
                "1-1: the board on 1-1 is t1000e, which lnflash knows at level expected",
                "the word yes typed at that question, by a person at the prompt or piped in; \
                 --yes answers every other confirmation and deliberately not this one",
                "Nothing was written, and it was not touched: it is still running what it ran \
                 before.",
            ] {
                assert!(said.contains(needle), "{needle:?} missing from:\n{said}");
            }
            assert!(!said.contains("1200-baud touch"), "{said}");
            assert!(!bench.touched(), "the port was set to 1200 baud: {said}");
            assert_eq!(*bench.heard.lock().unwrap(), 0, "not a byte was sent");
        }
    }

    #[test]
    fn a_running_expected_board_with_a_yes_is_asked_once_then_touched_and_flashed() {
        let bench = running_t1000e();
        let mut ui = Fake::typing(&["yes"]);
        let opts = Options {
            boards: vec!["t1000e".into()],
            ..dfu_opts()
        };
        let bus = run_on_bundle(&bench, &mut ui, &opts, expected_t1000e_bundle());
        let said = ui.transcript();
        assert!(bench.touched(), "{said}");
        assert!(bus.refused.is_empty(), "{said}");
        assert_eq!(bus.outcomes[0].board, "t1000e");
        assert_eq!(
            bench.board.lock().unwrap().ended,
            Some(Ended::Activated),
            "{said}"
        );
        // Asked once, before the touch, and not again in the bootloader.
        let question = "1-1: t1000e is at level expected";
        assert_eq!(said.matches(question).count(), 1, "{said}");
        let asked = said.find(question).unwrap();
        let touch = said.find("1-1: 1200-baud touch").unwrap();
        assert!(asked < touch, "{said}");
        assert_eq!(bus.exit_code(), EXIT_UNCONFIRMED, "{said}");
    }

    #[test]
    fn a_dry_run_on_a_running_expected_board_says_the_question_and_touches_nothing() {
        let bench = running_t1000e();
        let mut ui = Fake::agreeing();
        let opts = Options {
            dry_run: true,
            boards: vec!["t1000e".into()],
            ..dfu_opts()
        };
        let bus = run_on_bundle(&bench, &mut ui, &opts, expected_t1000e_bundle());
        let said = ui.transcript();
        assert_eq!(bus.skipped, ["1-1"], "{said}");
        assert_eq!(bus.exit_code(), EXIT_CONFIRMED, "{said}");
        for needle in [
            "1-1: would ask: 1-1: t1000e is at level expected",
            "1-1: would be refused: the board on 1-1 is t1000e",
            "it was not touched: it is still running",
        ] {
            assert!(said.contains(needle), "{needle:?} missing from:\n{said}");
        }
        assert!(!said.contains("would touch"), "{said}");
        assert!(!bench.touched());
    }

    #[test]
    fn a_running_id_shared_across_levels_is_asked_after_the_touch_with_the_way_back() {
        // No entry shares a running ID today. If a verified and an expected
        // entry ever do, only the bootloader can say which level applies:
        // nothing is asked before the touch, that is said, and a no after
        // it names the way back.
        let catalogue = Catalogue::builtin_with_all(&[
            (
                "candidate_usb = [\"1209:0002\"]",
                "candidate_usb = [\"1209:0002\", \"1209:0004\"]",
            ),
            (
                "what that reading gave.\nlevel     = \"verified\"",
                "what that reading gave.\nlevel     = \"expected\"",
            ),
        ])
        .unwrap();
        let sys = TempDir::new().unwrap();
        t1000e_bus(sys.path(), false);
        let candidate = find_candidates(&catalogue, &Sysfs::new(sys.path()))
            .unwrap()
            .remove(0);
        let both = ["t1000e".to_string(), "rak4631".to_string()];
        let mut ui = Fake::default();
        let answered = level_before_touch(&catalogue, &candidate, &both, &mut ui).unwrap();
        let said = ui.transcript();
        assert!(answered.is_empty(), "{said}");
        assert!(
            said.contains("gives to rak4631 (verified), t1000e (expected)"),
            "{said}"
        );
        assert!(
            said.contains("the question comes after the touch"),
            "{said}"
        );
        assert!(!said.contains("Type yes"), "{said}");

        let err = touched_at_level(
            Error::NeedsConfirmation {
                port: "1-1".into(),
                board: "t1000e".into(),
                level: "expected",
                left: Left::InBootloader,
            },
            &candidate,
        );
        let text = err.to_string();
        assert!(
            text.contains("This run touched it into its bootloader"),
            "{text}"
        );
        assert!(text.contains("press RESET once"), "{text}");
        // Only the expected one named: asked before the touch after all.
        let mut ui = Fake::default();
        let err = level_before_touch(&catalogue, &candidate, &["t1000e".to_string()], &mut ui)
            .unwrap_err();
        assert!(err.to_string().contains("it was not touched"), "{err}");
    }

    #[test]
    fn a_verified_running_board_is_asked_nothing_before_the_touch() {
        let sys = TempDir::new().unwrap();
        t1000e_bus(sys.path(), false);
        let catalogue = catalogue();
        let candidate = find_candidates(&catalogue, &Sysfs::new(sys.path()))
            .unwrap()
            .remove(0);
        let mut ui = Fake::default();
        assert!(level_before_touch(&catalogue, &candidate, &[], &mut ui)
            .unwrap()
            .is_empty());
        assert!(ui.said.is_empty(), "{}", ui.transcript());
    }

    // -----------------------------------------------------------------
    // #476: the zero-config flash, one test per path of the audit table
    // in docs/src/concepts/lnode-flashing.md
    // -----------------------------------------------------------------

    /// What a person running plain `lnflash` gets: every default, with the
    /// waits short enough for a test.
    fn zero_config_opts() -> Options {
        Options {
            appear_within: Duration::from_secs(2),
            banner_budget: Duration::from_millis(300),
            ..Options::default()
        }
    }

    #[test]
    fn zero_config_flashes_a_running_lnode_without_a_question() {
        // A refusing person: any question put would stop the flash.
        let bench = running_t1000e();
        let mut ui = Fake::refusing();
        let bus = run_on(&bench, &mut ui, &zero_config_opts());
        let said = ui.transcript();
        assert!(ui.questions.is_empty(), "asked: {:?}\n{said}", ui.questions);
        assert!(bench.touched(), "{said}");
        assert!(bus.refused.is_empty() && bus.skipped.is_empty(), "{said}");
        assert_eq!(bus.outcomes[0].board, "t1000e");
        assert_eq!(
            bench.board.lock().unwrap().ended,
            Some(Ended::Activated),
            "{said}"
        );
        assert!(said.contains("1-1: sent the application"), "{said}");
    }

    #[test]
    fn zero_config_flashes_a_board_found_in_its_bootloader_without_a_question() {
        let (_dir, catalogue, manifest) = t1000e_bundle();
        let bench = t1000e_in_bootloader(Bootloader::new(S140_7_3_0));
        let mut ui = Fake::refusing();
        let bus = run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            &mut ui,
            &zero_config_opts(),
            &FakeDrives { root: false },
        )
        .unwrap();
        let said = ui.transcript();
        assert!(ui.questions.is_empty(), "asked: {:?}\n{said}", ui.questions);
        assert!(bus.refused.is_empty(), "{said}");
        assert_eq!(bus.outcomes[0].board, "t1000e");
        assert_eq!(
            bench.board.lock().unwrap().ended,
            Some(Ended::Activated),
            "{said}"
        );
        // Back without a transport port: the radio step says what it could
        // not do instead of asking or guessing.
        assert!(
            said.contains("its radio settings were neither read nor sent"),
            "{said}"
        );
    }

    #[test]
    fn ask_brings_the_overwrite_question_back_and_a_no_writes_nothing() {
        let (_dir, catalogue, manifest) = t1000e_bundle();
        let bench = t1000e_in_bootloader(Bootloader::new(S140_7_3_0));
        let mut ui = Fake::refusing();
        let opts = Options {
            ask: true,
            ..zero_config_opts()
        };
        let bus = run_with(
            &catalogue,
            &manifest,
            &bench.sysfs(),
            &mut ui,
            &opts,
            &FakeDrives { root: false },
        )
        .unwrap();
        assert_eq!(
            ui.questions,
            ["\n1-1: overwrite the firmware on this t1000e?"]
        );
        assert_eq!(bus.refused, ["1-1"]);
        assert_eq!(*bench.heard.lock().unwrap(), 0, "not a byte was sent");
    }

    /// A bus with what lnflash must leave alone: a ThinkNode M9 (an ESP32-S3
    /// behind a CH340K, `1a86:7522`, one tty) and a hub (no tty at all).
    fn stranger_bus(root: &Path) {
        let m9 = root.join("1-2");
        sysfs_file(&m9, "idVendor", "1a86");
        sysfs_file(&m9, "idProduct", "7522");
        sysfs_file(&m9, "product", "USB Serial");
        sysfs_file(&root.join("1-2:1.0"), "bInterfaceNumber", "00");
        sysfs_file(&root.join("1-2:1.0"), "bInterfaceClass", "ff");
        // The usb-serial layout, one level below the interface.
        fs::create_dir_all(root.join("1-2:1.0/ttyUSB0/tty/ttyUSB0")).unwrap();
        let hub = root.join("usb1");
        sysfs_file(&hub, "idVendor", "1d6b");
        sysfs_file(&hub, "idProduct", "0002");
        sysfs_file(&hub, "product", "xHCI Host Controller");
        sysfs_file(&root.join("usb1:1.0"), "bInterfaceNumber", "00");
        sysfs_file(&root.join("usb1:1.0"), "bInterfaceClass", "09");
    }

    #[test]
    fn a_stranger_is_left_alone_with_one_line() {
        let (_dir, catalogue, manifest) = t1000e_bundle();
        let sys = TempDir::new().unwrap();
        stranger_bus(sys.path());
        let mut ui = Fake::refusing();
        let bus = run_with(
            &catalogue,
            &manifest,
            &Sysfs::with_dev(sys.path(), "/nonexistent-dev"),
            &mut ui,
            &zero_config_opts(),
            &FakeDrives { root: false },
        )
        .unwrap();
        let said = ui.transcript();
        let line = "  1-2 [1a86:7522] USB Serial: not a board lnflash flashes; left alone, \
                    nothing was sent to it.";
        assert_eq!(said.matches(line).count(), 1, "{said}");
        assert!(!said.contains("usb1"), "a hub is not a stranger: {said}");
        assert!(
            said.contains("No board lnflash knows is attached."),
            "{said}"
        );
        assert!(bus.is_empty(), "{bus:?}");
        assert!(ui.questions.is_empty(), "{:?}", ui.questions);

        // Beside a board lnflash does flash, the stranger still gets its
        // line and the board still gets flashed.
        let bench = running_t1000e();
        stranger_bus(bench.sys.path());
        let mut ui = Fake::refusing();
        let bus = run_on(&bench, &mut ui, &zero_config_opts());
        let said = ui.transcript();
        assert_eq!(said.matches(line).count(), 1, "{said}");
        assert_eq!(bus.outcomes[0].board, "t1000e", "{said}");
    }

    /// A fake `/proc` in which `pid`, named `name`, has `node` open.
    fn proc_holding(pid: u32, name: &str, node: &Path) -> TempDir {
        let proc = TempDir::new().unwrap();
        let fd = proc.path().join(pid.to_string()).join("fd");
        fs::create_dir_all(&fd).unwrap();
        symlink(node, fd.join("7")).unwrap();
        fs::write(
            proc.path().join(pid.to_string()).join("comm"),
            format!("{name}\n"),
        )
        .unwrap();
        proc
    }

    #[test]
    fn a_port_lnsd_holds_is_refused_with_the_holder_named() {
        // lnsd's serial medium opens every LNode's transport port, under
        // TIOCEXCL, which root's open walks through. The open must not be
        // taken as the port being free.
        let pty = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[("ttyACM2", &pty.slave_path)]);
        let proc = proc_holding(4242, "lnsd", &pty.slave_path);
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path())
            .with_proc(proc.path());
        let t114 = sysfs
            .devices()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "3-2.3.1")
            .unwrap();
        let err = open_transport(&sysfs, &t114, &dev.path().join("ttyACM2")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
        let text = err.to_string();
        for needle in [
            "is held by lnsd (pid 4242)",
            "nothing was sent",
            "stop lnsd or let it go of this board",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from: {text}");
        }

        // The flash flow's radio step on that board says it, rather than
        // reporting a silent board or failing without a word.
        let mut ui = Fake::refusing();
        let outcome = set_radio(&sysfs, &mut ui, &zero_config_opts(), &t114, "3-2.3.1").unwrap();
        assert_eq!(outcome, None);
        let said = ui.transcript();
        assert!(said.contains("held by lnsd (pid 4242)"), "{said}");
        assert!(said.contains("neither read nor sent"), "{said}");
        assert!(ui.questions.is_empty(), "{:?}", ui.questions);

        // Positive control: the same port with nobody else on it opens.
        let free = TempDir::new().unwrap();
        let sysfs = sysfs.with_proc(free.path());
        open_transport(&sysfs, &t114, &dev.path().join("ttyACM2")).unwrap();
    }

    #[test]
    fn another_program_on_the_port_is_named_without_blaming_lnsd() {
        let holders = [crate::usb::Holder {
            pid: 77,
            name: "minicom".into(),
        }];
        let text = held_port(Path::new("/dev/ttyACM2"), &holders, "nothing was read");
        assert!(text.contains("held by minicom (pid 77)"), "{text}");
        assert!(text.contains("close it, then run lnflash again"), "{text}");
        assert!(!text.contains("lnsd"), "{text}");
    }

    #[test]
    fn a_board_that_comes_back_naming_another_build_fails_the_flash() {
        // The verify step: the write went through, the board came back,
        // and its banner names a build that is not the one written.
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_dir, catalogue, manifest) = t1000e_bundle();
        let sys = TempDir::new().unwrap();
        t1000e_bus(sys.path(), true);
        let boot = crate::sys::testpty::Pty::open();
        let debug = crate::sys::testpty::Pty::open();
        let dev = dev_tree(&[
            ("ttyACM9", &boot.slave_path),
            ("ttyACM7", &debug.slave_path),
        ]);
        let (board, _heard) = serve_bootloader(boot, Bootloader::new(S140_7_3_0), sys.path());
        let stop = Arc::new(AtomicBool::new(false));
        let (halt, root, watched) = (stop.clone(), sys.path().to_path_buf(), board.clone());
        let speaker = std::thread::spawn(move || {
            // Once the image is activated the application binds its debug
            // port, and repeats the banner of a build nobody wrote.
            while watched.lock().unwrap().ended != Some(Ended::Activated) {
                if halt.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let iface = root.join("1-1:1.0");
            sysfs_file(&iface, "bInterfaceNumber", "00");
            sysfs_file(&iface, "bInterfaceClass", "02");
            fs::create_dir_all(iface.join("tty/ttyACM7")).unwrap();
            while !halt.load(Ordering::SeqCst) {
                debug.write_raw(b"[FW_BUILD] git_sha=aaaaaaaa dirty=false\r\n");
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let mut ui = Fake::refusing();
        let opts = Options {
            banner_budget: Duration::from_secs(1),
            radio: RadioPlan::Skip,
            ..zero_config_opts()
        };
        let bus = run_with(
            &catalogue,
            &manifest,
            &Sysfs::with_dev(sys.path(), dev.path()),
            &mut ui,
            &opts,
            &FakeDrives { root: false },
        )
        .unwrap();
        stop.store(true, Ordering::SeqCst);
        speaker.join().unwrap();
        let said = ui.transcript();
        assert!(ui.questions.is_empty(), "{:?}", ui.questions);
        assert_eq!(
            bus.outcomes[0].confirmation(),
            Confirmation::Failed,
            "{said}"
        );
        assert_eq!(bus.exit_code(), EXIT_FLASH_FAILED, "{said}");
        assert!(
            said.contains("the board reports git_sha=aaaaaaaa, not bb7c4f64"),
            "{said}"
        );
        assert!(said.contains("The write did not take."), "{said}");
    }

    fn leviculum_running(id: crate::usb::UsbId, manufacturer: Option<&str>) -> Candidate {
        Candidate {
            device: crate::usb::Device {
                name: "3-2.3.1".into(),
                id,
                serial: None,
                manufacturer: manufacturer.map(str::to_string),
                product: Some("T114".into()),
                interfaces: Vec::new(),
            },
            in_bootloader: false,
            hint: "t114".into(),
        }
    }

    #[test]
    fn only_our_running_image_on_our_id_names_the_board() {
        let catalogue = catalogue();
        let ours = crate::usb::UsbId::new(0x1209, 0x0001);
        let heltec_stock = crate::usb::UsbId::new(0x239a, 0x8071);
        assert_eq!(
            running_ours(&catalogue, &leviculum_running(ours, Some("leviculum"))).as_deref(),
            Some("t114")
        );
        // Heltec's stock ID, or our ID under somebody else's strings, or a
        // board already in its bootloader: nothing is named.
        assert_eq!(
            running_ours(
                &catalogue,
                &leviculum_running(heltec_stock, Some("leviculum"))
            ),
            None
        );
        assert_eq!(
            running_ours(&catalogue, &leviculum_running(ours, Some("pid.codes"))),
            None
        );
        assert_eq!(
            running_ours(&catalogue, &leviculum_running(ours, None)),
            None
        );
        let mut in_bootloader = leviculum_running(ours, Some("leviculum"));
        in_bootloader.in_bootloader = true;
        assert_eq!(running_ours(&catalogue, &in_bootloader), None);
    }

    #[test]
    fn a_shared_board_id_is_settled_by_our_running_image_and_asked_otherwise() {
        let (_dir, catalogue, manifest) = bundle();
        let info = infouf2::parse(T114_INFO);
        // Re-flash of a T114 that ran our T114 image: no question.
        let mut ui = Fake::refusing();
        let confirmed = confirm_identity_after(
            &catalogue,
            &manifest,
            &info,
            "3-2.4",
            None,
            &[],
            Some("t114"),
            &mut ui,
        )
        .unwrap();
        assert_eq!(confirmed.name(), "t114");
        assert!(ui.questions.is_empty(), "{:?}", ui.questions);
        assert!(
            ui.transcript().contains("no question"),
            "{}",
            ui.transcript()
        );

        // A board found in its bootloader, or running stock firmware: the
        // one question the zero-config run still puts.
        let mut ui = Fake::refusing();
        let err = confirm_identity_after(
            &catalogue,
            &manifest,
            &info,
            "3-2.4",
            None,
            &[],
            None,
            &mut ui,
        )
        .unwrap_err();
        assert!(matches!(err, Error::AmbiguousBoardId { .. }), "{err:?}");
        assert_eq!(ui.questions.len(), 1, "{:?}", ui.questions);

        // A running image naming another board settles nothing.
        let mut ui = Fake::refusing();
        assert!(confirm_identity_after(
            &catalogue,
            &manifest,
            &info,
            "3-2.4",
            None,
            &[],
            Some("rak4631"),
            &mut ui,
        )
        .is_err());
        assert_eq!(ui.questions.len(), 1);
    }

    // The radio step's rule (#476): keep what the board stored, send the
    // estimate over the shipped profile, send nothing that is already there.

    /// The fixture T114 at 3-2.3.1 with its transport port a pty, answered
    /// by `stub`.
    fn radio_board(
        stub: impl FnOnce(&crate::sys::testpty::Pty),
    ) -> (TempDir, Sysfs, Device, crate::sys::testpty::Pty) {
        let pty = crate::sys::testpty::Pty::open();
        stub(&pty);
        let dev = dev_tree(&[("ttyACM2", &pty.slave_path)]);
        let sysfs = Sysfs::with_dev(crate::sysfs_fixture::materialized(), dev.path());
        let t114 = sysfs
            .devices()
            .unwrap()
            .into_iter()
            .find(|d| d.name == "3-2.3.1")
            .unwrap();
        (dev, sysfs, t114, pty)
    }

    /// A board that reports `running` to the radio query and acks a config.
    fn reporting_stub(
        pty: &crate::sys::testpty::Pty,
        seen: crate::envelope::testing::Seen,
        running: RadioSettings,
    ) {
        use crate::envelope::testing::FIRMWARE_ACCEPTS;
        use leviculum_core::envelope::{
            classify_control_frame, encode_ack, encode_capability_report, encode_radio_report,
            ControlAction, TYPE_RADIO_CONFIG,
        };
        crate::sys::testpty::spawn_stub(pty, move |frame| {
            seen.lock().unwrap().push(frame.to_vec());
            match classify_control_frame(frame, FIRMWARE_ACCEPTS) {
                ControlAction::CapabilityQuery => Some(encode_capability_report(FIRMWARE_ACCEPTS)),
                ControlAction::RadioQuery => Some(encode_radio_report(&running.to_wire())),
                ControlAction::RadioConfig(_) => Some(encode_ack(TYPE_RADIO_CONFIG)),
                _ => None,
            }
        });
    }

    fn estimated(name: &str, why: &str) -> Options {
        Options {
            radio: RadioPlan::Estimate(crate::region::Estimate {
                preset: radio::preset(name).unwrap(),
                why: why.into(),
            }),
            ..zero_config_opts()
        }
    }

    #[test]
    fn the_estimate_keeps_settings_the_board_already_stored() {
        let seen = crate::envelope::testing::seen();
        let (_dev, sysfs, t114, _pty) =
            radio_board(|pty| crate::envelope::testing::envelope_firmware_stub(pty, seen.clone()));
        let mut ui = Fake::refusing();
        let opts = estimated("us915", "time zone America/New_York (US)");
        assert_eq!(
            set_radio(&sysfs, &mut ui, &opts, &t114, "3-2.3.1").unwrap(),
            None
        );
        let said = ui.transcript();
        assert!(
            said.contains("runs stored radio settings, not the shipped profile"),
            "{said}"
        );
        assert!(
            said.contains("freq=867500000 bw=250000 sf=9 cr=4/6): kept"),
            "{said}"
        );
        assert_eq!(crate::envelope::testing::radio_config_frame(&seen), None);
        assert!(ui.questions.is_empty(), "{:?}", ui.questions);
    }

    #[test]
    fn the_estimate_goes_to_a_board_on_the_shipped_profile() {
        let seen = crate::envelope::testing::seen();
        let (_dev, sysfs, t114, _pty) =
            radio_board(|pty| reporting_stub(pty, seen.clone(), radio::EU868));
        let mut ui = Fake::refusing();
        let opts = estimated("us915", "time zone America/New_York (US)");
        let outcome = set_radio(&sysfs, &mut ui, &opts, &t114, "3-2.3.1")
            .unwrap()
            .unwrap();
        let us915 = radio::preset("us915").unwrap().settings;
        assert_eq!(outcome.settings, us915);
        assert_eq!(outcome.taken, radio::ConfigTaken::Running);
        let sent = crate::envelope::testing::radio_config_frame(&seen).unwrap();
        assert_eq!(sent.frequency_hz, 914_875_000);
        let said = ui.transcript();
        assert!(
            said.contains("radio set to us915 from the time zone America/New_York (US)"),
            "{said}"
        );
        assert!(said.contains("15.247(a)(2)"), "the FCC note: {said}");
        assert!(ui.questions.is_empty(), "{:?}", ui.questions);
    }

    #[test]
    fn a_board_already_on_the_estimate_is_sent_nothing() {
        let seen = crate::envelope::testing::seen();
        let (_dev, sysfs, t114, _pty) =
            radio_board(|pty| reporting_stub(pty, seen.clone(), radio::EU868));
        let mut ui = Fake::refusing();
        let opts = estimated("eu868", "time zone Europe/Berlin (DE)");
        assert_eq!(
            set_radio(&sysfs, &mut ui, &opts, &t114, "3-2.3.1").unwrap(),
            None
        );
        let said = ui.transcript();
        assert!(
            said.contains("radio on eu868 already (time zone Europe/Berlin (DE)), nothing to send"),
            "{said}"
        );
        assert_eq!(crate::envelope::testing::radio_config_frame(&seen), None);
    }
}
