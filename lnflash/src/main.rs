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
use lnflash::radio::{self, RadioChoice, RadioPlan, RadioSettings};
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
                  Once a board is back up it is offered the EU868 radio defaults, or the \
                  settings the --radio-* flags name. The board stores what it is given, so it \
                  comes back up on that frequency after a reset and after the next flash.\n\n\
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

    /// Frequency in Hz for the radio settings written after the flash.
    /// Giving any --radio-* value skips the prompt; the ones not given keep
    /// their EU868 default.
    #[arg(long, value_name = "HZ")]
    radio_freq: Option<u32>,

    /// Bandwidth in Hz. One of 7810, 10420, 15630, 20830, 31250, 41670,
    /// 62500, 125000, 250000, 500000.
    #[arg(long, value_name = "HZ")]
    radio_bw: Option<u32>,

    /// Spreading factor, 7 to 12.
    #[arg(long, value_name = "SF")]
    radio_sf: Option<u8>,

    /// Coding-rate denominator, 5 to 8 (5 is 4/5).
    #[arg(long, value_name = "CR")]
    radio_cr: Option<u8>,

    /// Transmit power in dBm, -9 to 22.
    #[arg(long, value_name = "DBM", allow_hyphen_values = true)]
    radio_txpower: Option<i32>,

    /// Region/band preset. Reserved: the presets are not defined yet, so
    /// this refuses rather than guessing a band for somebody.
    #[arg(long, value_name = "NAME")]
    radio_preset: Option<String>,

    /// Leave the board's radio configuration alone. It then comes up on
    /// whatever it had stored, or on the compiled default if it had nothing.
    #[arg(long)]
    no_radio: bool,

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

/// What the `--radio-*` flags say, before any board is touched.
///
/// Resolved up front on purpose: an impossible `--radio-sf 3` has to stop the
/// run at the command line, not after a board has been written and is
/// waiting for a configuration the firmware would refuse.
fn radio_plan(cli: &Cli) -> Result<RadioPlan, Box<dyn std::error::Error>> {
    if let Some(name) = &cli.radio_preset {
        // TODO(presets): the flag exists so scripts can be written against
        // it; the region/band table it needs is still being researched.
        return Err(format!("presets not yet defined (asked for {name:?})").into());
    }
    if cli.no_radio {
        return Ok(RadioPlan::Skip);
    }
    let given = [
        cli.radio_freq.is_some(),
        cli.radio_bw.is_some(),
        cli.radio_sf.is_some(),
        cli.radio_cr.is_some(),
        cli.radio_txpower.is_some(),
    ];
    if !given.iter().any(|given| *given) {
        return Ok(RadioPlan::Ask);
    }
    // Every field the flags did not name keeps its EU868 value. Ignoring a
    // --radio-sf given without --radio-freq would be worse than either
    // refusing it or honouring it, and honouring it is what the user meant.
    let settings = RadioSettings {
        frequency_hz: cli.radio_freq.unwrap_or(radio::EU868.frequency_hz),
        bandwidth_hz: cli.radio_bw.unwrap_or(radio::EU868.bandwidth_hz),
        sf: cli.radio_sf.unwrap_or(radio::EU868.sf),
        cr: cli.radio_cr.unwrap_or(radio::EU868.cr),
        tx_power_dbm: match cli.radio_txpower {
            Some(dbm) => {
                radio::check_tx_power(dbm)?;
                dbm as i8
            }
            None => radio::EU868.tx_power_dbm,
        },
    };
    settings.check()?;
    Ok(RadioPlan::Fixed(RadioChoice::Custom(settings)))
}

fn run(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let radio = radio_plan(cli)?;
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
        radio,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(args: &[&str]) -> Result<RadioPlan, String> {
        let cli = Cli::try_parse_from(std::iter::once("lnflash").chain(args.iter().copied()))
            .map_err(|err| err.to_string())?;
        radio_plan(&cli).map_err(|err| err.to_string())
    }

    #[test]
    fn no_radio_flag_leaves_the_choice_to_the_prompt() {
        assert_eq!(plan(&[]).unwrap(), RadioPlan::Ask);
        assert_eq!(plan(&["--yes"]).unwrap(), RadioPlan::Ask);
    }

    #[test]
    fn a_frequency_on_the_command_line_skips_the_prompt() {
        let RadioPlan::Fixed(RadioChoice::Custom(settings)) =
            plan(&["--radio-freq", "867100000"]).unwrap()
        else {
            panic!("a stated frequency has to decide the question");
        };
        assert_eq!(settings.frequency_hz, 867_100_000);
        // Everything not stated keeps its EU868 value.
        assert_eq!(settings.bandwidth_hz, radio::EU868.bandwidth_hz);
        assert_eq!(settings.sf, radio::EU868.sf);
        assert_eq!(settings.cr, radio::EU868.cr);
        assert_eq!(settings.tx_power_dbm, radio::EU868.tx_power_dbm);
    }

    #[test]
    fn every_field_can_be_stated_including_a_negative_power() {
        let RadioPlan::Fixed(RadioChoice::Custom(settings)) = plan(&[
            "--radio-freq",
            "433175000",
            "--radio-bw",
            "62500",
            "--radio-sf",
            "11",
            "--radio-cr",
            "8",
            "--radio-txpower",
            "-9",
        ])
        .unwrap() else {
            panic!("the flags decide");
        };
        assert_eq!(
            settings,
            RadioSettings {
                frequency_hz: 433_175_000,
                bandwidth_hz: 62_500,
                sf: 11,
                cr: 8,
                tx_power_dbm: -9,
            }
        );
    }

    #[test]
    fn a_radio_flag_on_its_own_still_skips_the_prompt() {
        // Honouring --radio-sf without --radio-freq beats silently ignoring
        // a value the user typed.
        let RadioPlan::Fixed(RadioChoice::Custom(settings)) = plan(&["--radio-sf", "12"]).unwrap()
        else {
            panic!("a stated spreading factor has to be used");
        };
        assert_eq!(settings.sf, 12);
        assert_eq!(settings.frequency_hz, radio::EU868.frequency_hz);
    }

    #[test]
    fn an_impossible_flag_stops_the_run_before_a_board_is_touched() {
        for args in [
            vec!["--radio-sf", "3"],
            vec!["--radio-cr", "9"],
            vec!["--radio-bw", "100000"],
            vec!["--radio-txpower", "30"],
            vec!["--radio-freq", "1"],
        ] {
            assert!(plan(&args).is_err(), "{args:?} was accepted");
        }
    }

    #[test]
    fn a_preset_refuses_rather_than_guessing_a_band() {
        let err = plan(&["--radio-preset", "eu868"]).unwrap_err();
        assert!(err.contains("presets not yet defined"), "{err}");
        assert!(err.contains("eu868"), "{err}");
    }

    #[test]
    fn the_radio_step_can_be_left_out_altogether() {
        assert_eq!(plan(&["--no-radio"]).unwrap(), RadioPlan::Skip);
    }
}
