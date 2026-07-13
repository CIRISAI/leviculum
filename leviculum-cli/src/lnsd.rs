//! lnsd - Reticulum daemon
//!
//! This is the main daemon process that runs the Reticulum network stack.
//! Equivalent to rnsd in the Python implementation.

use std::path::PathBuf;

use clap::{ArgAction, Parser};
use tracing::info;
// (tracing-subscriber init goes through leviculum_std's
// event_log::install_global_subscriber so the LEVICULUM_EVENT_LOG
// env var is honoured for structured-event capture.  Codeberg #39
// piece 1.)

use leviculum_std::config::Config;
use leviculum_std::Reticulum;

#[derive(Parser, Debug)]
#[command(name = "lnsd")]
#[command(author, version = env!("LEVICULUM_VERSION"), about = "Reticulum network daemon")]
struct Args {
    /// Path to Reticulum config directory (like Python rnsd --config)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Storage directory path (default: <config_dir>/storage).
    ///
    /// Long-only: rnsd's `-s` means `--service`, so `-s` is reserved for that
    /// and storage keeps only its long spelling (a Leviculum extension).
    #[arg(long)]
    storage: Option<PathBuf>,

    /// Running as a service (rnsd `-s/--service`). Recognised for drop-in
    /// compatibility with `rnsd -s`. lnsd keeps logging to stdout, which
    /// systemd/journald captures; it does not redirect to a log file.
    #[arg(short, long)]
    service: bool,

    /// Increase log verbosity (repeat for more: -v debug, -vv trace)
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,

    /// Decrease log verbosity (repeat for less: -q warn, -qq error)
    #[arg(short, long, action = ArgAction::Count)]
    quiet: u8,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // RUST_LOG env takes precedence; otherwise use -v/-q flags
    let default_filter = match (args.verbose as i8) - (args.quiet as i8) {
        2.. => "trace",
        1 => "debug",
        0 => "info",
        -1 => "warn",
        _ => "error",
    };
    leviculum_std::event_log::install_global_subscriber(default_filter);

    info!("Starting lnsd v{}", env!("CARGO_PKG_VERSION"));
    if args.service {
        info!("Service mode (-s): logging to stdout for systemd/journald capture");
    }

    // --config is a directory (like Python rnsd), config file is {dir}/config
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let config_file = config_dir.join("config");
    let storage_path = args.storage.unwrap_or_else(|| config_dir.join("storage"));

    info!("Config dir: {}", config_dir.display());
    info!("Config file: {}", config_file.display());
    info!("Storage: {}", storage_path.display());

    // Load and configure Reticulum
    let mut config = if config_file.exists() {
        Config::load(&config_file)?
    } else {
        Config::default()
    };
    config.reticulum.storage_path = Some(storage_path);

    // Resource receive-window policy (Codeberg #85): env var, not a config
    // key, because the config format is shared with Python rnsd.
    let window_policy = leviculum_std::resource_policy::resource_window_policy_from_env();

    let mut rns = Reticulum::with_config_daemon(config, window_policy)?;
    rns.start().await?;

    info!("Reticulum daemon running");

    // Wait for shutdown signal (SIGINT or SIGTERM), dump diagnostics on SIGUSR1
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigusr1 = signal(SignalKind::user_defined1())?;
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => { info!("Received SIGINT"); break; }
                _ = sigterm.recv() => { info!("Received SIGTERM"); break; }
                _ = sigusr1.recv() => {
                    let dump = rns.diagnostic_dump();
                    eprint!("{}", dump);
                }
            }
        }
    }

    info!("Shutting down...");
    rns.stop().await?;

    Ok(())
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        let mut v = vec!["lnsd"];
        v.extend_from_slice(args);
        Args::try_parse_from(v).expect("parse ok")
    }

    #[test]
    fn service_short_and_long_set_flag() {
        assert!(parse(&["-s"]).service);
        assert!(parse(&["--service"]).service);
        assert!(!parse(&[]).service);
    }

    #[test]
    fn storage_is_long_only_and_s_is_not_storage() {
        // --storage still parses to a path.
        let a = parse(&["--storage", "/tmp/store"]);
        assert_eq!(a.storage, Some(PathBuf::from("/tmp/store")));
        assert!(!a.service);

        // -s is the service flag, never storage: `-s /tmp/store` treats
        // /tmp/store as a positional (which lnsd has none of) -> error,
        // and -s itself does not consume a value as storage.
        assert!(parse(&["-s"]).storage.is_none());
        assert!(Args::try_parse_from(["lnsd", "-s", "/tmp/store"]).is_err());
    }
}
