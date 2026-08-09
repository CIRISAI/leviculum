//! `lnflash` — unpack, run one binary, get a board running our firmware.
//!
//! The whole tool in one sentence: find what is attached, bring it into its
//! bootloader, confirm from the bootloader what it actually is, check the
//! SoftDevice precondition, and only then write. See [`lnflash`] for the
//! design and `docs/src/concepts/lnode-flashing.md` for the evidence behind
//! every constant.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use lnflash::flow::{self, Options};
use lnflash::manifest;
use lnflash::ui::{Assumed, Console, Ui};
use lnflash::usb::{Sysfs, SYSFS_USB_DEVICES};

#[derive(Parser, Debug)]
#[command(
    name = "lnflash",
    version,
    about = "Flash an LNode board from the bundle beside this binary",
    long_about = "Finds attached boards, brings each into its bootloader, confirms what it is \
                  from the bootloader itself, checks the SoftDevice precondition, and writes \
                  our firmware.\n\n\
                  Needs root: the bootloader's drive is a root:disk block device and \
                  automounting assumes a desktop stack a headless host does not have.\n\n\
                  No network access, ever — everything it writes is in the bundle."
)]
struct Cli {
    /// Where the bundle is. Otherwise: $LNFLASH_BUNDLE, then next to this
    /// binary, then /usr/share/lnflash.
    #[arg(long, value_name = "PATH")]
    bundle: Option<PathBuf>,

    /// Only flash this board, and refuse if what is attached is another one.
    #[arg(long, value_name = "NAME")]
    board: Option<String>,

    /// Report what is attached and what would happen. Changes nothing — not
    /// even rebooting a board into its bootloader, which is already a change.
    #[arg(long)]
    dry_run: bool,

    /// Answer yes to every confirmation. For automation; fails rather than
    /// waits if a board needs a physical double-tap.
    #[arg(long)]
    yes: bool,

    /// Print less.
    #[arg(long, short)]
    quiet: bool,

    /// Check the bundle's manifest and payload checksums, then exit.
    #[arg(long)]
    check_bundle: bool,

    /// Read USB devices from here instead of /sys/bus/usb/devices. For
    /// testing against a captured tree; no board is ever touched through it.
    #[arg(long, value_name = "DIR", hide = true)]
    sysfs: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("lnflash: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let dir = manifest::locate(cli.bundle.as_deref())?;
    let manifest = manifest::load(&dir)?;

    let mut console;
    let mut assumed;
    let ui: &mut dyn Ui = if cli.yes {
        assumed = Assumed::new(cli.quiet);
        &mut assumed
    } else {
        console = Console::new(cli.quiet);
        &mut console
    };

    ui.say(&format!(
        "lnflash {} — bundle {} from {}, carrying {}",
        env!("CARGO_PKG_VERSION"),
        manifest.bundle.version,
        manifest
            .bundle
            .built
            .as_deref()
            .unwrap_or("an unknown date"),
        manifest.names().join(", ")
    ));

    if cli.check_bundle {
        manifest.verify_all()?;
        ui.say("Every image in this bundle matches its recorded checksum.");
        return Ok(ExitCode::SUCCESS);
    }

    // Say this before enumerating rather than after a failed mount: a user
    // who forgot sudo should learn it in the first line, not the last.
    if !cli.dry_run && !lnflash::sys::is_root() {
        ui.say(
            "Not running as root. The bootloader drive is a root:disk block device, so this \
             will get as far as identifying boards and then stop. Re-run with sudo to write.",
        );
    }

    let sysfs = match &cli.sysfs {
        Some(path) => Sysfs::new(path),
        None => Sysfs::new(SYSFS_USB_DEVICES),
    };
    let opts = Options {
        board: cli.board.clone(),
        dry_run: cli.dry_run,
        ..Options::default()
    };

    let outcomes = flow::run(&manifest, &sysfs, ui, &opts)?;
    if outcomes.is_empty() {
        // Nothing was flashed. That is a clean exit for --dry-run and for an
        // empty bus, and a failure for a run that was supposed to write.
        return Ok(if cli.dry_run {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let good = outcomes.iter().filter(|o| o.is_good()).count();
    ui.say(&format!(
        "\n{good} of {} board(s) confirmed running the firmware in this bundle.",
        outcomes.len()
    ));
    for outcome in outcomes.iter().filter(|o| !o.is_good()) {
        ui.say(&format!(
            "  {} ({}): not confirmed — {:?}",
            outcome.port, outcome.board, outcome.verdict
        ));
    }
    Ok(if good == outcomes.len() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
