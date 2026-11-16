//! `lnmgmt` — Leviculum's remote-management client for the verbs that change
//! a node's state (OTA stage 1, Codeberg #235).
//!
//! `lnstatus` stays the read-only tool, held byte-identical to `rnstatus` for
//! drop-in use; a state-changing verb there would be a surprise one mistyped
//! flag away. This binary is where such verbs live, so `/dfu` is not the only
//! one forever. Today it has one:
//!
//! `lnmgmt dfu -R <transport identity hash> -i <identity file> [-w <secs>]`
//! asks a board, over the mesh and on any carrier, to enter its bootloader's
//! BLE DFU mode, and prints the board's typed verdict. The request is the
//! same flow `lnstatus -R` runs, through the local shared instance
//! (`leviculum_std::remote_dfu::send_dfu`, on
//! `leviculum_std::remote_status::request_on_management`).
//!
//! `-R` is the board's transport identity hash, the same value `lnstatus -R`
//! takes; the management destination is derived from it.
//!
//! Exit status: 0 accepted, 1 refused (not listed, or no identity), 3 no
//! answer within the budget, 4 any other failure (no shared instance, the
//! link closed, an unreadable reply), 2 a usage error.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use leviculum_cli::daemon_rpc;
use leviculum_cli::remote_mgmt;
use leviculum_std::config_dir::ConfigDirChoice;
use leviculum_std::remote_dfu::{send_dfu, DfuOutcome, DfuVerdict};

/// Exit status for an accepted command.
const EXIT_ACCEPTED: u8 = 0;
/// Exit status when the board refused.
const EXIT_REFUSED: u8 = 1;
/// Exit status for a usage error, the same as clap's own.
const EXIT_USAGE: u8 = 2;
/// Exit status when nothing answered within the budget.
const EXIT_TIMEOUT: u8 = 3;
/// Exit status for every other failure.
const EXIT_FAILED: u8 = 4;

/// Default budget, the same as `lnstatus -R`'s
/// (`RNS.Transport.PATH_REQUEST_TIMEOUT`, 15 s).
const DEFAULT_TIMEOUT_SECS: f64 = 15.0;

/// What `lnmgmt dfu --help` says after the options: the bootloader never
/// leaves BLE DFU by itself (61.5 minutes measured on a T114, 2026-11-10),
/// and the way out without a hand on the board is not a mesh verb, because
/// a board in its bootloader is off the mesh. lnmgmt has no Bluetooth path,
/// so the help names the tool that has one.
const DFU_AFTER_HELP: &str = "\
The board stays in BLE DFU until an image arrives or it is reset; it does not \
time out. It advertises as <board>-OTA (HT-n5262-OTA on a T114) from its \
application's Bluetooth address plus one. To bring it back without a transfer, \
run scripts/dfu-exit.py from the Leviculum source tree on a host within \
Bluetooth range (BlueZ, as root): it writes the Legacy DFU reset to the \
bootloader and the application comes back intact. A power cycle or the reset \
button does the same.";

/// Reticulum remote management: verbs that change a node's state.
#[derive(Parser, Debug)]
#[command(
    name = "lnmgmt",
    version = env!("LEVICULUM_VERSION"),
    about = "Reticulum remote management"
)]
struct Args {
    /// path to alternative Reticulum config directory
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// shared-instance name to use (default: from config, else "default")
    #[arg(long, global = true)]
    instance_name: Option<String>,

    /// fail when no shared instance answers instead of starting lnsd as a child
    #[arg(long, global = true)]
    no_start: bool,

    /// verbose output
    #[arg(short = 'v', long = "verbose", global = true)]
    verbose: bool,

    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand, Debug)]
enum Verb {
    /// put a board into its bootloader's BLE DFU mode
    #[command(after_help = DFU_AFTER_HELP)]
    Dfu(DfuArgs),
}

#[derive(clap::Args, Debug)]
struct DfuArgs {
    /// transport identity hash of the board (32 hex characters)
    #[arg(short = 'R', value_parser = parse_remote)]
    remote: [u8; 16],

    /// path to the management identity the board's allow-list holds
    #[arg(short = 'i')]
    identity: PathBuf,

    /// seconds to wait for the path and the reply
    #[arg(short = 'w', default_value_t = DEFAULT_TIMEOUT_SECS, value_parser = parse_timeout)]
    timeout: f64,
}

fn parse_remote(s: &str) -> Result<[u8; 16], String> {
    remote_mgmt::parse_identity_hash(s)
}

fn parse_timeout(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(t) if t > 0.0 && t.is_finite() => Ok(t),
        Ok(_) => Err("the timeout must be a positive number of seconds".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// What the user reads for a verdict, and the exit status it ends with.
fn verdict_words(verdict: DfuVerdict) -> (&'static str, u8) {
    match verdict {
        DfuVerdict::Accepted => ("accepted: the board resets into BLE DFU now", EXIT_ACCEPTED),
        DfuVerdict::NotListed => ("refused: not listed", EXIT_REFUSED),
        DfuVerdict::NoIdentity => ("refused: no identity", EXIT_REFUSED),
    }
}

/// The line and exit status for every way `/dfu` can end.
fn outcome_words(outcome: &DfuOutcome, timeout: Duration) -> (String, u8) {
    match outcome {
        DfuOutcome::Verdict(verdict) => {
            let (words, code) = verdict_words(*verdict);
            (words.to_string(), code)
        }
        DfuOutcome::TimedOut(error) => {
            use leviculum_std::remote_status::ManagementError;
            let what = match error {
                ManagementError::PathTimedOut => "no path to the board",
                _ => "no reply from the board",
            };
            (
                format!("timed out: {what} within {} s", timeout.as_secs_f64()),
                EXIT_TIMEOUT,
            )
        }
        DfuOutcome::Failed(message) => (format!("failed: {message}"), EXIT_FAILED),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match &args.verb {
        Verb::Dfu(dfu) => ExitCode::from(run_dfu(&args, dfu).await),
    }
}

async fn run_dfu(args: &Args, dfu: &DfuArgs) -> u8 {
    let identity = match remote_mgmt::load_management_identity(&dfu.identity) {
        Ok(identity) => identity,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_USAGE;
        }
    };

    // Which directory, and why: every failure below names it (order 1675).
    let choice = ConfigDirChoice::resolve(args.config.as_deref());
    let config_dir = choice.dir.clone();
    let config = daemon_rpc::load_config(&config_dir);
    let instance_name = args
        .instance_name
        .clone()
        .or_else(|| config.as_ref().map(|c| c.reticulum.instance_name.clone()))
        .unwrap_or_else(|| "default".to_string());

    // Find the shared instance, or start lnsd as our child (#471), held to
    // the end of the verb like every client tool does.
    let _instance = match leviculum_std::instance::for_tool(
        &config_dir,
        &instance_name,
        args.no_start,
        leviculum_std::instance::Report::from_verbose(args.verbose),
    ) {
        Ok(instance) => instance,
        Err(e) => {
            eprintln!("No shared RNS instance available");
            eprintln!("({e})");
            eprintln!("{}", choice.hint_for(&instance_name));
            return EXIT_FAILED;
        }
    };
    let config = config.or_else(|| daemon_rpc::load_config(&config_dir));

    let (mut node, mut events) = match remote_mgmt::attach_to_shared_instance(
        &instance_name,
        daemon_rpc::resolve_storage_path(&config_dir, config.as_ref()),
    )
    .await
    {
        Ok(attached) => attached,
        Err(e) => {
            eprintln!("No shared RNS instance available");
            eprintln!("({e})");
            eprintln!("{}", choice.hint_for(&instance_name));
            return EXIT_FAILED;
        }
    };

    let timeout = Duration::from_secs_f64(dfu.timeout);
    let outcome = send_dfu(
        &node,
        &mut events,
        &dfu.remote,
        &identity,
        timeout,
        !args.verbose,
    )
    .await;
    // `stop` drains the LINKCLOSE the request queued, as in `lnstatus -R`.
    let _ = node.stop().await;

    let (line, code) = outcome_words(&outcome, timeout);
    if code == EXIT_ACCEPTED || code == EXIT_REFUSED {
        println!("{line}");
    } else {
        eprintln!("{line}");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_std::remote_status::ManagementError;

    const HASH: &str = "000102030405060708090a0b0c0d0e0f";

    fn parse(argv: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("lnmgmt").chain(argv.iter().copied()))
    }

    #[test]
    fn dfu_takes_remote_identity_and_timeout() {
        let args = parse(&["dfu", "-R", HASH, "-i", "/tmp/id", "-w", "3.5"]).unwrap();
        let Verb::Dfu(dfu) = args.verb;
        assert_eq!(
            dfu.remote,
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert_eq!(dfu.identity, PathBuf::from("/tmp/id"));
        assert_eq!(dfu.timeout, 3.5);
    }

    /// 1660: the help says how a board gets out of DFU again, and names
    /// the tool, because nothing in lnmgmt can do it.
    #[test]
    fn dfu_help_names_the_way_back() {
        use clap::CommandFactory;
        let mut cmd = Args::command();
        let dfu = cmd.find_subcommand_mut("dfu").unwrap();
        let help = dfu.render_long_help().to_string();
        assert!(help.contains("scripts/dfu-exit.py"), "{help}");
        assert!(help.contains("does not time out"), "{help}");
        assert!(help.contains("-OTA"), "{help}");
    }

    #[test]
    fn dfu_defaults_to_the_lnstatus_budget() {
        let Verb::Dfu(dfu) = parse(&["dfu", "-R", HASH, "-i", "id"]).unwrap().verb;
        assert_eq!(dfu.timeout, DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn global_options_go_before_or_after_the_verb() {
        let args = parse(&["--config", "/etc/r", "dfu", "-R", HASH, "-i", "id", "-v"]).unwrap();
        assert_eq!(args.config, Some(PathBuf::from("/etc/r")));
        assert!(args.verbose);
        let args = parse(&["dfu", "-R", HASH, "-i", "id", "--no-start"]).unwrap();
        assert!(args.no_start);
    }

    #[test]
    fn dfu_refuses_what_it_cannot_send() {
        // Both -R and -i are mandatory: a board refuses an unidentified
        // link anyway, so asking without one is a usage error, not a request.
        assert!(parse(&["dfu", "-i", "id"]).is_err());
        assert!(parse(&["dfu", "-R", HASH]).is_err());
        // A hash of the wrong length or with non-hex digits.
        assert!(parse(&["dfu", "-R", "abcd", "-i", "id"]).is_err());
        assert!(parse(&["dfu", "-R", &"z".repeat(32), "-i", "id"]).is_err());
        // A budget that is not a positive number of seconds.
        for bad in ["0", "-1", "nan", "inf", "soon"] {
            assert!(
                parse(&["dfu", "-R", HASH, "-i", "id", "-w", bad]).is_err(),
                "-w {bad} must be refused"
            );
        }
        // No verb at all, and a verb that does not exist.
        assert!(parse(&[]).is_err());
        assert!(parse(&["status", "-R", HASH]).is_err());
    }

    #[test]
    fn usage_errors_exit_with_the_usage_status() {
        let err = parse(&["dfu", "-R", "abcd", "-i", "id"]).unwrap_err();
        assert_eq!(err.exit_code(), i32::from(EXIT_USAGE));
    }

    #[test]
    fn verdicts_in_words_and_only_accepted_exits_zero() {
        let words = |v| outcome_words(&DfuOutcome::Verdict(v), Duration::from_secs(15));
        assert_eq!(
            words(DfuVerdict::Accepted),
            ("accepted: the board resets into BLE DFU now".to_string(), 0)
        );
        assert_eq!(
            words(DfuVerdict::NotListed),
            ("refused: not listed".to_string(), EXIT_REFUSED)
        );
        assert_eq!(
            words(DfuVerdict::NoIdentity),
            ("refused: no identity".to_string(), EXIT_REFUSED)
        );
    }

    #[test]
    fn a_timeout_says_so_and_exits_non_zero() {
        let budget = Duration::from_secs(15);
        let (line, code) = outcome_words(
            &DfuOutcome::TimedOut(ManagementError::RequestTimedOut),
            budget,
        );
        assert_eq!(line, "timed out: no reply from the board within 15 s");
        assert_eq!(code, EXIT_TIMEOUT);
        let (line, code) =
            outcome_words(&DfuOutcome::TimedOut(ManagementError::PathTimedOut), budget);
        assert_eq!(line, "timed out: no path to the board within 15 s");
        assert_eq!(code, EXIT_TIMEOUT);
        let (line, code) = outcome_words(&DfuOutcome::Failed("link closed".into()), budget);
        assert_eq!(line, "failed: link closed");
        assert_eq!(code, EXIT_FAILED);
    }
}
