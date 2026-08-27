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
use leviculum_core::envelope::{TelemetryTargetWire, TELEMETRY_PROFILE_STATION};
use lnflash::flow::{self, Options};
use lnflash::manifest;
use lnflash::radio::{self, RadioChoice, RadioPlan, RadioSettings};
use lnflash::telemetry::{self, TelemetryPlan};
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
                  Once a board is back up it is offered the eu868 radio defaults (ReticulumNet \
                  consensus), a preset menu (eu868, us915, au915, custom), or the settings the \
                  --radio-preset / --radio-* flags name. The board stores what it is given, so \
                  it comes back up on that frequency after a reset and after the next flash.\n\n\
                  It then asks whether the board should send telemetry. The default is no, and \
                  a yes needs exactly one input: the LXMF address to report to. Telemetry is \
                  configuration rather than firmware, so --set-telemetry does the same thing \
                  later without reflashing.\n\n\
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

    /// Tell every running LNode what time it is, then exit. No flashing:
    /// uses the control envelope on the transport port; firmware from
    /// before the envelope reports itself as such.
    #[arg(long)]
    set_time: bool,

    /// Set the on-air transmit spacing, in milliseconds, on every running
    /// LNode, then exit. No flashing. The board leaves this gap between the
    /// end of one packet's airtime and the key-up of the next; 0 is the
    /// compiled default and imposes nothing. A bench instrument for #345:
    /// the value is not persisted, so a reset restores the default.
    #[arg(long, value_name = "MS", conflicts_with_all = ["set_time", "set_telemetry"])]
    set_tx_spacing: Option<u16>,

    /// Set the transmit power, in dBm, on every running LNode, then exit.
    /// No flashing. Reads the board's current radio settings first and sends
    /// them back with only the power changed, so nothing else moves; a board
    /// that cannot report them is left alone. Unlike --set-tx-spacing this
    /// IS persisted — a reset comes back on the value set here. -9 to 22;
    /// anything outside that range is clamped by the board, which says so on
    /// its debug port.
    #[arg(
        long,
        value_name = "DBM",
        allow_hyphen_values = true,
        conflicts_with_all = ["set_time", "set_telemetry", "set_tx_spacing"]
    )]
    set_tx_power: Option<i32>,

    /// Configure the telemetry target on every running LNode, then exit.
    /// No flashing — activation is configuration, not firmware. Takes the
    /// --telemetry / --telemetry-profile / --telemetry-key / --no-telemetry
    /// flags, and asks if none of them is given.
    #[arg(long, conflicts_with = "set_time")]
    set_telemetry: bool,

    /// Send position telemetry to this LXMF address. Implies yes to the
    /// prompt. 32 hex characters; spaces, colons and upper case are fine.
    #[arg(long, value_name = "ADDRESS")]
    telemetry: Option<String>,

    /// Which cadence the telemetry uses: tracker (movement-driven) or
    /// station (slow stationary heartbeat, the default).
    #[arg(long, value_name = "PROFILE")]
    telemetry_profile: Option<String>,

    /// The target's public key, 128 hex characters. Optional and rarely
    /// needed: without it the node resolves the key over the air, which is
    /// the common case.
    #[arg(long, value_name = "KEY")]
    telemetry_key: Option<String>,

    /// Switch telemetry off: clear whatever target the board has stored.
    #[arg(long)]
    no_telemetry: bool,

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

    /// Region preset: eu868 (ReticulumNet consensus), us915 (US community,
    /// see the FCC note it prints), or au915. Skips the prompt. Cannot be
    /// combined with the --radio-* value flags.
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
    if let Some(name) = &cli.radio_preset {
        // A preset and explicit values are two ways to state one
        // configuration; honouring one and dropping the other would do
        // silently what the user should decide.
        if given.iter().any(|given| *given) {
            return Err(
                "--radio-preset and the --radio-freq/-bw/-sf/-cr/-txpower flags are two ways \
                 to state one configuration; pick one"
                    .into(),
            );
        }
        return Ok(RadioPlan::Fixed(RadioChoice::Preset(radio::preset(name)?)));
    }
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

/// What the `--telemetry*` flags say, before any board is touched.
///
/// Resolved up front for the same reason [`radio_plan`] is: a mistyped
/// address has to stop the run at the command line, not after a board has
/// been written and is waiting for a frame the firmware would refuse.
fn telemetry_plan(cli: &Cli) -> Result<TelemetryPlan, Box<dyn std::error::Error>> {
    if cli.no_telemetry {
        // "Off" and "on" are two ways to answer one question; honouring one
        // and dropping the other would decide silently what the user should.
        if cli.telemetry.is_some() || cli.telemetry_key.is_some() || cli.telemetry_profile.is_some()
        {
            return Err(
                "--no-telemetry switches telemetry off and the other --telemetry-* flags \
                 switch it on; pick one"
                    .into(),
            );
        }
        return Ok(TelemetryPlan::Clear);
    }
    let profile = match &cli.telemetry_profile {
        Some(name) => telemetry::parse_profile(name)?,
        None => TELEMETRY_PROFILE_STATION,
    };
    let Some(address) = &cli.telemetry else {
        if cli.telemetry_key.is_some() {
            return Err(
                "--telemetry-key is the key of a target, so it needs the --telemetry address \
                 it belongs to"
                    .into(),
            );
        }
        // A profile on its own is not an answer to "send telemetry?", only
        // to "which cadence" — so the prompt still runs, and the address it
        // collects gets this profile.
        return Ok(TelemetryPlan::Ask { profile });
    };
    Ok(TelemetryPlan::Fixed(TelemetryTargetWire {
        profile,
        dest_hash: telemetry::parse_address(address)?,
        public_key: match &cli.telemetry_key {
            Some(key) => Some(telemetry::parse_key(key)?),
            None => None,
        },
    }))
}

fn run(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let radio = radio_plan(cli)?;
    let telemetry = telemetry_plan(cli)?;
    // The board catalogue is compiled in and always available. The bundle is
    // located only by the paths that need an image, so a session that merely
    // configures a board that is already running never asks for one
    // (Codeberg #342) — and a session that does need one still fails with the
    // bundle error, naming everywhere it looked.
    let catalogue = manifest::Catalogue::builtin()?;

    let mut console;
    let mut assumed;
    let ui: &mut dyn Ui = if cli.yes {
        assumed = Assumed::new(cli.quiet);
        &mut assumed
    } else {
        console = Console::new(cli.quiet);
        &mut console
    };

    if cli.set_time {
        let sysfs = match &cli.sysfs {
            Some(path) => Sysfs::new(path),
            None => Sysfs::new(SYSFS_USB_DEVICES),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "the host clock is before 1970; refusing to teach a board that")?
            .as_secs();
        let all_took_it = flow::set_time(&catalogue, &sysfs, ui, now)?;
        return Ok(if all_took_it {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    if let Some(spacing_ms) = cli.set_tx_spacing {
        let sysfs = match &cli.sysfs {
            Some(path) => Sysfs::new(path),
            None => Sysfs::new(SYSFS_USB_DEVICES),
        };
        let all_took_it = flow::set_tx_spacing(&catalogue, &sysfs, ui, spacing_ms)?;
        return Ok(if all_took_it {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    if let Some(dbm) = cli.set_tx_power {
        // The wire field is one signed byte; the CLI takes an i32 so a value
        // outside it is named here rather than wrapping into a plausible one.
        // The *range* check is the board's — a value the chip cannot do is
        // clamped and announced, never refused (see #349).
        let dbm = i8::try_from(dbm)
            .map_err(|_| format!("--set-tx-power {dbm} does not fit the one-byte wire field"))?;
        let sysfs = match &cli.sysfs {
            Some(path) => Sysfs::new(path),
            None => Sysfs::new(SYSFS_USB_DEVICES),
        };
        let all_took_it = flow::set_tx_power(&catalogue, &sysfs, ui, dbm)?;
        return Ok(if all_took_it {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    if cli.set_telemetry {
        let sysfs = match &cli.sysfs {
            Some(path) => Sysfs::new(path),
            None => Sysfs::new(SYSFS_USB_DEVICES),
        };
        let all_took_it = flow::set_telemetry(&catalogue, &sysfs, ui, &telemetry)?;
        return Ok(if all_took_it {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let dir = manifest::locate(cli.bundle.as_deref())?;
    let manifest = manifest::load(&dir, &catalogue)?;

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
        telemetry,
        ..Options::default()
    };

    let outcomes = flow::run(&catalogue, &manifest, &sysfs, ui, &opts)?;
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
    fn a_named_preset_decides_the_question_with_the_tables_values() {
        for (name, frequency_hz) in [
            ("eu868", 869_463_000),
            ("us915", 914_875_000),
            ("au915", 925_875_000),
        ] {
            let RadioPlan::Fixed(RadioChoice::Preset(preset)) =
                plan(&["--radio-preset", name]).unwrap()
            else {
                panic!("{name} has to decide the question");
            };
            assert_eq!(preset.name, name);
            assert_eq!(preset.settings.frequency_hz, frequency_hz);
        }
    }

    #[test]
    fn eu433_is_refused_with_the_reason_rather_than_shipped_4_db_hot() {
        let err = plan(&["--radio-preset", "eu433"]).unwrap_err();
        assert!(err.contains("10 dBm"), "{err}");
        assert!(err.contains("not offered"), "{err}");
    }

    #[test]
    fn an_unknown_preset_is_refused_with_the_choices() {
        let err = plan(&["--radio-preset", "mars"]).unwrap_err();
        assert!(err.contains("mars"), "{err}");
        assert!(err.contains("eu868, us915, au915"), "{err}");
    }

    #[test]
    fn a_preset_and_an_explicit_field_together_are_a_usage_error() {
        let err = plan(&["--radio-preset", "eu868", "--radio-freq", "867100000"]).unwrap_err();
        assert!(err.contains("pick one"), "{err}");
        // Any of the five value flags collides, not just the frequency.
        let err = plan(&["--radio-preset", "us915", "--radio-sf", "9"]).unwrap_err();
        assert!(err.contains("pick one"), "{err}");
    }

    #[test]
    fn the_radio_step_can_be_left_out_altogether() {
        assert_eq!(plan(&["--no-radio"]).unwrap(), RadioPlan::Skip);
    }

    // -----------------------------------------------------------------
    // Telemetry (Codeberg #236)
    // -----------------------------------------------------------------

    const ADDRESS: &str = "a7b2c3d4e5f60718293a4b5c6d7e8f90";
    const ADDRESS_BYTES: [u8; 16] = [
        0xa7, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18, 0x29, 0x3a, 0x4b, 0x5c, 0x6d, 0x7e, 0x8f,
        0x90,
    ];

    fn tplan(args: &[&str]) -> Result<TelemetryPlan, String> {
        let cli = Cli::try_parse_from(std::iter::once("lnflash").chain(args.iter().copied()))
            .map_err(|err| err.to_string())?;
        telemetry_plan(&cli).map_err(|err| err.to_string())
    }

    #[test]
    fn no_telemetry_flag_leaves_the_choice_to_the_prompt_at_the_default_profile() {
        // Defaults first: with nothing said, the tool asks, and the profile
        // a bare "yes" would use is station.
        for args in [vec![], vec!["--yes"]] {
            assert_eq!(
                tplan(&args).unwrap(),
                TelemetryPlan::Ask {
                    profile: leviculum_core::envelope::TELEMETRY_PROFILE_STATION
                }
            );
        }
    }

    #[test]
    fn an_address_on_the_command_line_skips_the_prompt() {
        let TelemetryPlan::Fixed(target) = tplan(&["--telemetry", ADDRESS]).unwrap() else {
            panic!("a stated address has to decide the question");
        };
        assert_eq!(target.dest_hash, ADDRESS_BYTES);
        assert_eq!(target.profile, TELEMETRY_PROFILE_STATION);
        // Hash-only is the common case, so the key stays absent unless asked
        // for: the node resolves it over the air.
        assert_eq!(target.public_key, None);
    }

    #[test]
    fn the_profile_flag_names_the_cadence_on_both_paths() {
        let TelemetryPlan::Fixed(target) =
            tplan(&["--telemetry", ADDRESS, "--telemetry-profile", "tracker"]).unwrap()
        else {
            panic!("the flags decide");
        };
        assert_eq!(
            target.profile,
            leviculum_core::envelope::TELEMETRY_PROFILE_TRACKER
        );
        // Without an address the profile is not an answer to "send
        // telemetry?", so the prompt still runs — carrying the profile.
        assert_eq!(
            tplan(&["--telemetry-profile", "tracker"]).unwrap(),
            TelemetryPlan::Ask {
                profile: leviculum_core::envelope::TELEMETRY_PROFILE_TRACKER
            }
        );
    }

    #[test]
    fn a_key_can_be_given_and_travels_with_the_target() {
        let key = "5e".repeat(64);
        let TelemetryPlan::Fixed(target) =
            tplan(&["--telemetry", ADDRESS, "--telemetry-key", &key]).unwrap()
        else {
            panic!("the flags decide");
        };
        assert_eq!(target.public_key, Some([0x5Eu8; 64]));
    }

    #[test]
    fn no_telemetry_is_an_explicit_clear_rather_than_a_skip() {
        assert_eq!(tplan(&["--no-telemetry"]).unwrap(), TelemetryPlan::Clear);
    }

    #[test]
    fn switching_it_on_and_off_in_one_command_is_a_usage_error() {
        for args in [
            vec!["--no-telemetry", "--telemetry", ADDRESS],
            vec!["--no-telemetry", "--telemetry-profile", "tracker"],
            vec!["--no-telemetry", "--telemetry-key", "5e"],
        ] {
            let err = tplan(&args).unwrap_err();
            assert!(err.contains("pick one"), "{args:?}: {err}");
        }
    }

    #[test]
    fn a_key_without_the_address_it_belongs_to_is_a_usage_error() {
        let err = tplan(&["--telemetry-key", &"5e".repeat(64)]).unwrap_err();
        assert!(err.contains("--telemetry address"), "{err}");
    }

    #[test]
    fn a_mistyped_value_stops_the_run_before_a_board_is_touched() {
        let err = tplan(&["--telemetry", "a7b2"]).unwrap_err();
        assert!(err.contains("32 hex characters"), "{err}");
        let err = tplan(&["--telemetry", ADDRESS, "--telemetry-key", "5e"]).unwrap_err();
        assert!(err.contains("128 hex characters"), "{err}");
        let err = tplan(&["--telemetry", ADDRESS, "--telemetry-profile", "beacon"]).unwrap_err();
        assert!(err.contains("tracker"), "{err}");
    }

    #[test]
    fn the_two_configure_only_sessions_are_not_one_command() {
        // --set-time and --set-telemetry both end the run after talking to
        // the boards, so asking for both would silently drop one.
        let err = tplan(&["--set-time", "--set-telemetry"]).unwrap_err();
        assert!(err.contains("cannot be used with"), "{err}");
    }

    // -----------------------------------------------------------------
    // Transmit spacing (Codeberg #345)
    // -----------------------------------------------------------------

    fn spacing(args: &[&str]) -> Result<Option<u16>, String> {
        let cli = Cli::try_parse_from(std::iter::once("lnflash").chain(args.iter().copied()))
            .map_err(|err| err.to_string())?;
        Ok(cli.set_tx_spacing)
    }

    #[test]
    fn no_spacing_flag_means_the_session_does_not_run_at_all() {
        // The control: without the flag nothing about the board's transmit
        // path is touched, so a plain flash cannot change the spacing.
        assert_eq!(spacing(&[]).unwrap(), None);
        assert_eq!(spacing(&["--yes"]).unwrap(), None);
    }

    #[test]
    fn a_spacing_on_the_command_line_is_carried_verbatim() {
        // Including the two ends of the range: 0 is the default and has to
        // be a value rather than an absent flag.
        for ms in [0u16, 15, 60, 76, u16::MAX] {
            assert_eq!(
                spacing(&["--set-tx-spacing", &ms.to_string()]).unwrap(),
                Some(ms)
            );
        }
    }

    #[test]
    fn a_spacing_that_does_not_fit_the_wire_is_a_usage_error() {
        for value in ["-1", "70000", "sixty"] {
            assert!(
                spacing(&["--set-tx-spacing", value]).is_err(),
                "{value} was accepted"
            );
        }
    }

    #[test]
    fn the_configure_only_sessions_are_not_one_command() {
        // Each of them ends the run after talking to the boards, so any
        // pair would silently drop one.
        for args in [
            vec!["--set-time", "--set-tx-spacing", "60"],
            vec!["--set-telemetry", "--set-tx-spacing", "60"],
            vec!["--set-time", "--set-tx-power", "14"],
            vec!["--set-telemetry", "--set-tx-power", "14"],
            vec!["--set-tx-spacing", "60", "--set-tx-power", "14"],
        ] {
            let err = spacing(&args).unwrap_err();
            assert!(err.contains("cannot be used with"), "{args:?}: {err}");
        }
    }

    // -----------------------------------------------------------------
    // Transmit power (Codeberg #349)
    // -----------------------------------------------------------------

    fn txpower(args: &[&str]) -> Result<Option<i32>, String> {
        let cli = Cli::try_parse_from(std::iter::once("lnflash").chain(args.iter().copied()))
            .map_err(|err| err.to_string())?;
        Ok(cli.set_tx_power)
    }

    #[test]
    fn no_tx_power_flag_means_the_session_does_not_run_at_all() {
        assert_eq!(txpower(&[]).unwrap(), None);
        assert_eq!(txpower(&["--yes"]).unwrap(), None);
    }

    /// The whole -9..=22 range reaches the flow, negatives included. The
    /// leading `-` is why the argument needs `allow_hyphen_values`, and a
    /// parser that lost it would turn -9 into an unknown flag rather than
    /// into a power.
    #[test]
    fn every_power_the_part_accepts_parses_including_the_negatives() {
        for dbm in -9i32..=22 {
            assert_eq!(
                txpower(&["--set-tx-power", &dbm.to_string()]).unwrap(),
                Some(dbm),
                "{dbm} dBm"
            );
        }
    }

    /// Out of range is NOT a usage error: the board clamps and says so, which
    /// is the project's rule for a value the hardware cannot do. Only a value
    /// that does not fit the one-byte wire field is refused, and that
    /// refusal is in `run`, not in the parser.
    #[test]
    fn an_out_of_range_power_is_carried_to_the_board_rather_than_refused_here() {
        assert_eq!(txpower(&["--set-tx-power", "37"]).unwrap(), Some(37));
        assert_eq!(txpower(&["--set-tx-power", "-20"]).unwrap(), Some(-20));
        assert!(txpower(&["--set-tx-power", "loud"]).is_err());
    }
}
