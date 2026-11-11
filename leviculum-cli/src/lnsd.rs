//! lnsd - Reticulum daemon
//!
//! This is the main daemon process that runs the Reticulum network stack.
//! Equivalent to rnsd in the Python implementation.

use std::path::PathBuf;

use clap::{ArgAction, Parser};
use tracing::{info, warn};
// (tracing-subscriber init goes through leviculum_std's
// event_log::install_global_subscriber so the LEVICULUM_EVENT_LOG
// env var is honoured for structured-event capture.  Codeberg #39
// piece 1.)

use leviculum_std::config::{Config, LNSD_DEFAULT_CONFIG};
use leviculum_std::Reticulum;

/// Measured live heap, under the default-off `heap-accounting` feature.
///
/// Why a build flag and not a permanent fixture: the shim adds two relaxed
/// atomics to every allocation, and a daemon that carries other people's
/// traffic should pay that only when someone is asking the question. Why it
/// exists at all: the dump's own total is modelled — flat multipliers per
/// collection — and at the heap bench's table shape the model over-reported
/// live data by 1.92x, so "resident is N times the data" computed from it is
/// a statement about the model. This number is the allocator's own balance:
/// bytes handed out minus bytes taken back.
///
/// The allocator is only counted, never replaced: every call is forwarded to
/// `System` with the layout it arrived with, so a node built with this runs
/// the same mallocng, takes the same paths and fragments the same way as one
/// built without it.
#[cfg(all(feature = "heap-accounting", not(feature = "alloc-mimalloc")))]
#[global_allocator]
static ALLOC: leviculum_std::heap_accounting::CountingAllocator =
    leviculum_std::heap_accounting::CountingAllocator;

/// mimalloc instead of the libc allocator, under the `alloc-mimalloc`
/// feature, default since 766. Retention, not an uncounted owner, is what the
/// soak node's census found between live heap and resident set (#437): musl's
/// mallocng let it grow by about 400 MB a day, mimalloc held it flat (508
/// built the variant that measured it). Counted, in the same shim, when
/// `heap-accounting` is on too, so the two variants' censuses compare.
#[cfg(all(feature = "heap-accounting", feature = "alloc-mimalloc"))]
#[global_allocator]
static ALLOC: leviculum_std::heap_accounting::CountingOver<mimalloc::MiMalloc> =
    leviculum_std::heap_accounting::CountingOver(mimalloc::MiMalloc);

#[cfg(all(not(feature = "heap-accounting"), feature = "alloc-mimalloc"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The line `heap_accounting::measured_live_line` prints, with this
/// binary's allocator named: only the binary knows which one it installed.
#[cfg(feature = "heap-accounting")]
fn measured_live_line() -> String {
    let allocator = if cfg!(feature = "alloc-mimalloc") {
        "mimalloc"
    } else {
        "system"
    };
    leviculum_std::heap_accounting::measured_live_line(allocator)
}

#[derive(Parser, Debug)]
#[command(name = "lnsd")]
#[command(author, version = env!("LEVICULUM_VERSION"), about = "Reticulum network daemon")]
struct Args {
    /// Path to Reticulum config directory (like Python rnsd --config)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Storage directory path (default: the config's `storage_path`, else
    /// <config_dir>/storage).
    ///
    /// Moves the daemon only: the client tools resolve storage from the
    /// config, so a storage directory that has to be shared with them
    /// belongs in the config file (Codeberg #241).
    ///
    /// Long-only: rnsd's `-s` means `--service`, so `-s` is reserved for that
    /// and storage keeps only its long spelling (a Leviculum extension).
    #[arg(long)]
    storage: Option<PathBuf>,

    /// Serve the shared instance under this name instead of the config's
    /// `instance_name` (a Leviculum extension, rnsd has none).
    ///
    /// A client tool that starts lnsd for an instance its config does not
    /// name passes it here (`leviculum_std::instance::find_or_start`), so the
    /// daemon it started answers on the name it waits for. The file is not
    /// rewritten.
    #[arg(long, value_name = "NAME")]
    instance_name: Option<String>,

    /// Running as a service (rnsd `-s/--service`). Recognised for drop-in
    /// compatibility with `rnsd -s`. lnsd keeps logging to stdout, which
    /// systemd/journald captures; it does not redirect to a log file.
    #[arg(short, long)]
    service: bool,

    /// Print an example configuration to stdout and exit (rnsd
    /// `--exampleconfig`). The output loads through lnsd's own config loader.
    #[arg(long)]
    exampleconfig: bool,

    /// Write the configuration lnsd runs without a file to `<config>/config`
    /// and exit: the file rnsd writes on its first start, byte for byte,
    /// followed by an `[auto]` section with the shipped defaults, which rnsd
    /// ignores. Refuses to replace an existing file.
    #[arg(long)]
    writeconfig: bool,

    /// Increase log verbosity (repeat for more: -v debug, -vv trace)
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,

    /// Decrease log verbosity (repeat for less: -q warn, -qq error)
    #[arg(short, long, action = ArgAction::Count)]
    quiet: u8,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Display, not Debug. Returning `Err` from `main` prints the
            // Debug form, which turned the shared-instance name clash into
            // `Error: Io(Os { code: 98, kind: AddrInUse, .. })` in the
            // journal — a struct dump where the operator needed a sentence.
            eprintln!("lnsd: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // --exampleconfig prints a config template and exits, before any logging
    // or stack init (rnsd.py:75-77).
    if args.exampleconfig {
        print!("{}", example_config());
        return Ok(());
    }

    // --config is a directory (like Python rnsd), config file is {dir}/config.
    // Resolved before the subscriber so we can peek the config loglevel.
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let config_file = config_dir.join("config");

    // --writeconfig, like --exampleconfig, acts before any logging or stack
    // init: it is a file operation, not a daemon start.
    if args.writeconfig {
        write_default_config(&config_file)?;
        println!(
            "Wrote the default configuration to {}",
            config_file.display()
        );
        return Ok(());
    }

    // Log-level precedence (matches rnsd, CLI overrides config):
    //   RUST_LOG > CLI -v/-q (when non-zero) > config [logging] loglevel > info.
    // RUST_LOG is applied by EnvFilter inside install_global_subscriber; here we
    // peek the config loglevel BEFORE installing the subscriber so it can seed
    // the default filter when no RUST_LOG / CLI verbosity is given.
    let rust_log_present = std::env::var("RUST_LOG").is_ok();
    let config_loglevel = if config_file.exists() {
        Config::peek_loglevel(&config_file)
    } else {
        None
    };
    let default_filter =
        resolve_default_filter(config_loglevel, args.verbose, args.quiet, rust_log_present);
    leviculum_std::event_log::install_global_subscriber(&default_filter);

    info!("Starting lnsd v{}", env!("CARGO_PKG_VERSION"));
    // Said at startup, because an operator reading a SIGUSR1 dump has to be
    // able to tell which build produced it without diffing two binaries.
    #[cfg(feature = "heap-accounting")]
    {
        info!("Heap accounting: on, SIGUSR1 reports measured live bytes beside the estimate");
        leviculum_std::heap_accounting::configure_sampler_from_env();
    }
    // Both ways, so a field node's journal names its allocator whichever
    // build it runs; build-deb.sh reads the mimalloc line off the binary.
    #[cfg(feature = "alloc-mimalloc")]
    info!("Allocator: mimalloc");
    #[cfg(not(feature = "alloc-mimalloc"))]
    info!("Allocator: system (libc)");
    if args.service {
        info!("Service mode (-s): logging to stdout for systemd/journald capture");
    }

    info!("Config dir: {}", config_dir.display());
    info!("Config file: {}", config_file.display());

    // Load and configure Reticulum. Without a file lnsd runs the
    // configuration rnsd would write on its first start, from memory: one
    // AutoInterface, a shared instance, transport off (#471 step 1).
    let (mut config, source) = Config::load_or_default(&config_file)?;
    if let Some(name) = args.instance_name.as_ref().filter(|n| !n.is_empty()) {
        info!("Instance name: {} (--instance-name)", name);
        config.reticulum.instance_name = name.clone();
    }
    // And writes it, the way rnsd does (`Reticulum.py:336`), the bytes
    // `--writeconfig` writes (#471, open question 1): a Python tool pointed
    // at this directory afterwards finds a file and writes its own stock
    // file nowhere, and the next start reads this one, every medium at its
    // shipped default in `[auto]`. This start keeps running what it loaded,
    // origin `default`. A directory lnsd may not write into is a warning,
    // not a failed start.
    let written = match source {
        leviculum_std::config::ConfigSource::File => "no",
        leviculum_std::config::ConfigSource::Default => match write_default_config(&config_file) {
            Ok(()) => "yes",
            Err(e) => {
                warn!(
                    "Could not write the first-start configuration to {}: {}",
                    config_file.display(),
                    e
                );
                "failed"
            }
        },
    };
    info!(
        event = "CONFIG",
        source = %source.as_str(),
        path = %leviculum_std::event_log::Scalar(&config_file.display().to_string()),
        written = written,
    );
    // The automatic layer (#471 step 2): the file's `[auto]` section, or the
    // shipped defaults without a file, unless the host marker or
    // LEVICULUM_NO_AUTO turns it off. Decided and stated once, before the
    // driver logs the interface sections it was handed.
    let auto_layer = leviculum_std::auto_layer::decide_for_host(source, config.auto);
    leviculum_std::auto_layer::apply(&mut config, auto_layer)?;
    auto_layer.log();
    config.reticulum.log_defaulted_keys();

    // Storage precedence: --storage > config `storage_path` > <config_dir>/storage.
    // The resolved path is written back so the builder, the log line above and
    // the client tools all name the same directory. Assigning the derived path
    // unconditionally is what discarded the config's own value (Codeberg #241).
    let storage_path = args
        .storage
        .unwrap_or_else(|| config.reticulum.resolve_storage_path(&config_dir));
    info!("Storage: {}", storage_path.display());
    // One daemon per storage directory (1638): a second lnsd under another
    // instance name would otherwise run on the same files. Held until exit.
    let _storage_lock = leviculum_std::storage_lock::StorageLock::take(
        &storage_path,
        &config.reticulum.instance_name,
    )?;
    // The servers medium (#471 step 3) keeps its choice in the storage
    // directory, so it is applied once that is known.
    let env_servers = std::env::var(leviculum_std::auto_servers::ENV_SERVERS).ok();
    leviculum_std::auto_servers::apply(
        &mut config,
        auto_layer,
        env_servers.as_deref(),
        Some(&storage_path),
    )?;
    config.reticulum.storage_path = Some(storage_path);
    // The serial medium (#471 step 5): the boards present now become
    // sections before the node is built, so they are logged and reported
    // like any other default section; a board plugged in later is found by
    // the rescan below.
    let mut auto_serial = leviculum_std::auto_serial::for_layer(auto_layer, &config);
    if let Some(scanner) = auto_serial.as_mut() {
        leviculum_std::auto_serial::apply(scanner, &mut config);
    }
    // The Bluetooth medium (#471 step 6): BlueZ is asked for an adapter
    // now, so a present one is a section like any other default section;
    // a missing or powered-off one is a warning, asked again below.
    let mut auto_bluetooth = leviculum_std::auto_bluetooth::for_layer(auto_layer, &config);
    if let Some(medium) = auto_bluetooth.as_mut() {
        leviculum_std::auto_bluetooth::apply(medium, &mut config).await;
    }

    // Resource receive-window policy (Codeberg #85): env var, not a config
    // key, because the config format is shared with Python rnsd.
    let window_policy = leviculum_std::resource_policy::resource_window_policy_from_env();

    // The two shutdown signals are taken before the daemon opens its
    // sockets: a client that connects the moment the shared instance
    // listens and stops the daemon at once (Loquax does, when it started
    // the daemon and then finds no terminal to draw in) must reach the
    // shutdown below. Taken only after the start, a SIGTERM in between
    // met the default action, which ended lnsd mid-start with no line in
    // its log and nothing persisted (1684: the log of a red
    // `with_an_lnsd_to_start_the_messenger...` ended at "Local server
    // listening", before the "Out-of-band control" line the registration
    // below prints). tokio holds a signal that arrives before the loop
    // waits for it.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    let mut rns = Reticulum::with_config_daemon(config, window_policy)?;
    rns.start().await?;

    info!("Reticulum daemon running");

    // The automatic media's finds after the start, handed to the loop
    // below, which attaches them.
    let (serial_tx, mut serial_rx) =
        tokio::sync::mpsc::channel::<leviculum_std::config::InterfaceConfig>(16);
    // The Bluetooth rescan: a missing or powered-off adapter is asked for
    // every `RESCAN` until one is taken; after that the BLE interface's own
    // retry loop owns it.
    if let Some(mut medium) = auto_bluetooth {
        if medium.wants_probe() {
            let tx = serial_tx.clone();
            tokio::spawn(async move {
                while medium.wants_probe() {
                    tokio::time::sleep(leviculum_std::auto_bluetooth::RESCAN).await;
                    if let Some(section) = leviculum_std::auto_bluetooth::check(&mut medium).await {
                        let _ = tx.send(section).await;
                        return;
                    }
                }
            });
        }
    }
    // The serial rescan: every `RESCAN` a blocking pass over the by-id
    // directory.
    if let Some(mut scanner) = auto_serial {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(leviculum_std::auto_serial::RESCAN).await;
                let pass = tokio::task::spawn_blocking(move || {
                    let found = scanner.scan();
                    (scanner, found)
                })
                .await;
                let Ok((back, found)) = pass else { return };
                scanner = back;
                for section in found {
                    if serial_tx.send(section).await.is_err() {
                        return;
                    }
                }
            }
        });
    } else {
        drop(serial_tx);
    }

    // Wait for shutdown signal (SIGINT or SIGTERM), dump diagnostics on
    // SIGUSR1, command a firmware reset on SIGUSR2, and command an
    // announce, a media silence or a media restore on the three real-time
    // numbers.
    //
    // SIGUSR2 joins an existing surface rather than inventing one: SIGUSR1
    // is already how this daemon is asked to do something out of band, and
    // a signal needs no socket, no authentication story and no wire format
    // to work identically for a daemon in a container (`docker kill
    // --signal=USR2`) and one on the host. The alternative was a verb on
    // the RPC server, which speaks Python `multiprocessing.connection` for
    // rnstatus/rnpath compatibility — a protocol whose whole point is that
    // it mirrors Python-RNS, and which would gain a verb Python-RNS has
    // no idea about.
    //
    // What they do: every firmware board this daemon holds gets the
    // commanded-reset frame, or the announce-now control frame
    // (`TYPE_ANNOUNCE`), on its data port. The daemon is the only process
    // that CAN send either, because it holds the port exclusively for its
    // whole run (periculum #255).
    //
    // Why the announce is a REAL-TIME signal and not a third named one:
    // the named spares are gone. SIGUSR1 and SIGUSR2 are taken, and SIGHUP
    // is the one signal this daemon must keep dying from — lnsd is
    // routinely run in a terminal, and a HUP that announced instead of
    // terminating would leave an orphan holding the boards' ports. The
    // real-time range is the one Linux reserves for exactly this: signals
    // with no prior meaning.
    //
    // It is a fixed NUMBER and not `SIGRTMIN`, for the reason
    // [`leviculum_std::interfaces::FIRMWARE_ANNOUNCE_SIGNAL`] states in
    // full: `SIGRTMIN` is a libc opinion (glibc 34, musl 35, both measured
    // on this host) and lnsd ships in both flavours, so a sender naming
    // the signal would resolve it against ITS libc and miss. The startup
    // line states the number so nothing has to be inferred from a source
    // file.
    {
        use tokio::signal::unix::{signal, SignalKind};
        let announce_signal = leviculum_std::interfaces::FIRMWARE_ANNOUNCE_SIGNAL;
        let silence_signal = leviculum_std::interfaces::FIRMWARE_MEDIA_SILENCE_SIGNAL;
        let restore_signal = leviculum_std::interfaces::FIRMWARE_MEDIA_RESTORE_SIGNAL;
        let mut sigusr1 = signal(SignalKind::user_defined1())?;
        let mut sigusr2 = signal(SignalKind::user_defined2())?;
        let mut announce = signal(SignalKind::from_raw(announce_signal))?;
        let mut media_silence = signal(SignalKind::from_raw(silence_signal))?;
        let mut media_restore = signal(SignalKind::from_raw(restore_signal))?;
        info!(
            "Out-of-band control: SIGUSR1 diagnostics, SIGUSR2 firmware reset, \
             signal {announce_signal} commanded announce, signal {silence_signal} media \
             silence, signal {restore_signal} media restore"
        );
        loop {
            tokio::select! {
                _ = sigint.recv() => { info!("Received SIGINT"); break; }
                _ = sigterm.recv() => { info!("Received SIGTERM"); break; }
                Some(section) = serial_rx.recv() => {
                    let name = section.name.clone();
                    match rns.spawn_interface(section) {
                        Ok(ids) => info!("Attached [[{}]] ({} interface(s))", name, ids.len()),
                        Err(e) => warn!("Could not attach [[{}]]: {}", name, e),
                    }
                }
                _ = sigusr1.recv() => {
                    let dump = rns.diagnostic_dump();
                    eprint!("{}", dump);
                    // Beside the dump and not inside it: the dump is
                    // leviculum-std's and is printed by every build, this
                    // line exists only in a build that can measure it.
                    #[cfg(feature = "heap-accounting")]
                    eprint!("{}", measured_live_line());
                }
                _ = sigusr2.recv() => {
                    let reached = leviculum_std::interfaces::request_firmware_reset();
                    // Said out loud in both directions: a daemon holding
                    // no board is a caller that signalled the wrong node,
                    // and silence would read as a reset that happened.
                    info!("Received SIGUSR2: commanded firmware reset on {reached} serial interface(s)");
                }
                _ = announce.recv() => {
                    let reached = leviculum_std::interfaces::request_firmware_announce();
                    // Same rule as the reset's count, and the same reason:
                    // a caller waiting for a client to learn this board's
                    // destination has to be able to tell "no board here"
                    // from "the announce went out and nobody heard it".
                    info!(
                        "Received signal {announce_signal}: commanded announce on \
                         {reached} serial interface(s)"
                    );
                }
                _ = media_silence.recv() => {
                    let reached =
                        leviculum_std::interfaces::request_firmware_media_silence();
                    // The same count, for the same reason the announce
                    // logs one: a scenario that takes a node off the air
                    // and asserts the mesh routed around it has to be
                    // able to tell "no board here" from "the carriers
                    // went off and the mesh carried on regardless".
                    info!(
                        "Received signal {silence_signal}: commanded media silence on \
                         {reached} serial interface(s)"
                    );
                }
                _ = media_restore.recv() => {
                    let reached =
                        leviculum_std::interfaces::request_firmware_media_restore();
                    info!(
                        "Received signal {restore_signal}: commanded media restore on \
                         {reached} serial interface(s)"
                    );
                }
            }
        }
    }

    info!("Shutting down...");
    rns.stop().await?;

    Ok(())
}

/// Map an RNS log level (0-7) to a tracing env-filter directive.
///
/// RNS levels: 0 critical, 1 error, 2 warning, 3 notice, 4 info, 5 verbose,
/// 6 debug, 7 extreme (reference/Reticulum/RNS/__init__.py:66-73). tracing has
/// no notice/verbose/extreme, so notice folds into info, verbose into debug,
/// and extreme into trace. Values above 7 map to trace.
fn rns_loglevel_to_filter(level: u8) -> &'static str {
    match level {
        0 | 1 => "error",
        2 => "warn",
        3 | 4 => "info",
        5 | 6 => "debug",
        _ => "trace",
    }
}

/// Map the CLI `-v/-q` net verbosity delta to a tracing directive, matching the
/// historical lnsd behaviour.
fn cli_verbosity_to_filter(delta: i8) -> &'static str {
    match delta {
        2.. => "trace",
        1 => "debug",
        0 => "info",
        -1 => "warn",
        _ => "error",
    }
}

/// Select the default tracing filter used when `RUST_LOG` is unset.
///
/// Precedence matches rnsd (CLI overrides config): `RUST_LOG` > CLI `-v/-q`
/// (when non-zero) > config `[logging] loglevel` > default `info`. `RUST_LOG`
/// itself is applied one layer up by `EnvFilter::try_from_default_env`, so when
/// `rust_log_present` is true this returned default is unused by the subscriber;
/// we still avoid letting config override it here.
fn resolve_default_filter(
    loglevel: Option<u8>,
    verbose: u8,
    quiet: u8,
    rust_log_present: bool,
) -> String {
    let delta = (verbose as i8) - (quiet as i8);
    if delta != 0 {
        // Explicit CLI verbosity wins over config loglevel.
        return cli_verbosity_to_filter(delta).to_string();
    }
    if !rust_log_present {
        if let Some(level) = loglevel {
            return rns_loglevel_to_filter(level).to_string();
        }
    }
    "info".to_string()
}

/// Write [`LNSD_DEFAULT_CONFIG`] to `path`, creating its directory as rnsd
/// does (`Reticulum.py:1200-1202`). An existing file is somebody's
/// configuration and is never replaced.
fn write_default_config(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Err(format!(
            "{} exists; --writeconfig does not replace a configuration",
            path.display()
        )
        .into());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // `create_new`: a file that appeared since the check above, a Python
    // tool's on a first start, is not replaced either.
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(LNSD_DEFAULT_CONFIG.as_bytes())?;
    Ok(())
}

/// A concise, representative example configuration, printed by
/// `--exampleconfig`. It is deliberately short (a `[reticulum]` block, a
/// `[logging]` level, a `[[Default Interface]]` AutoInterface, and a commented
/// TCPClient example) and MUST round-trip through [`Config::load`].
fn example_config() -> &'static str {
    r#"# Example Reticulum configuration for lnsd.
# Edit to add the interfaces and settings you need.

[reticulum]

# Enable Transport to route traffic for other peers and pass announces.
# Recommended for always-on, stationary transport nodes.
enable_transport = No

# Share this instance with other local programs over a local socket.
share_instance = Yes

[logging]

# Log level 0-7: 0 critical, 1 error, 2 warning, 3 notice,
# 4 info (default), 5 verbose, 6 debug, 7 extreme.
loglevel = 4

[interfaces]

  # Communicate with link-local Reticulum peers over UDP/IPv6.
  # Needs no routers or DHCP, only link-local IPv6 (on by default).
  [[Default Interface]]
    type = AutoInterface
    enabled = yes

  # Connect out to a remote TCP server interface. Uncomment and edit
  # target_host / target_port, then set enabled = yes.
  # [[TCP Client Interface]]
  #   type = TCPClientInterface
  #   enabled = no
  #   target_host = 127.0.0.1
  #   target_port = 4242

  # Join the Columba BLE mesh (LNode boards, phones) as a dual-role
  # BlueZ node. Needs bluetoothd and a BLE-capable adapter. Off by
  # default; uncomment and set enabled = yes to use it.
  # [[BLE Interface]]
  #   type = BLEInterface
  #   enabled = no
"#
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

    #[test]
    fn loglevel_precedence_cli_over_config() {
        // Config loglevel applies when neither CLI -v/-q nor RUST_LOG is set.
        assert_eq!(resolve_default_filter(Some(6), 0, 0, false), "debug");
        assert_eq!(resolve_default_filter(Some(2), 0, 0, false), "warn");
        // CLI -v wins over config loglevel.
        assert_eq!(resolve_default_filter(Some(2), 1, 0, false), "debug");
        // CLI -q wins over config loglevel.
        assert_eq!(resolve_default_filter(Some(6), 0, 2, false), "error");
        // RUST_LOG present -> config is not applied (EnvFilter uses RUST_LOG,
        // so the returned default is the CLI/default only).
        assert_eq!(resolve_default_filter(Some(6), 0, 0, true), "info");
        // No config, no CLI -> default info.
        assert_eq!(resolve_default_filter(None, 0, 0, false), "info");
    }

    #[test]
    fn rns_loglevel_maps_to_tracing() {
        assert_eq!(rns_loglevel_to_filter(0), "error");
        assert_eq!(rns_loglevel_to_filter(1), "error");
        assert_eq!(rns_loglevel_to_filter(2), "warn");
        assert_eq!(rns_loglevel_to_filter(3), "info");
        assert_eq!(rns_loglevel_to_filter(4), "info");
        assert_eq!(rns_loglevel_to_filter(5), "debug");
        assert_eq!(rns_loglevel_to_filter(6), "debug");
        assert_eq!(rns_loglevel_to_filter(7), "trace");
    }

    #[test]
    fn writeconfig_writes_the_template_and_never_replaces_a_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        // rnsd creates the directory too.
        let path = dir.path().join("fresh").join("config");
        write_default_config(&path).expect("write into a fresh directory");
        let written = std::fs::read(&path).expect("read back");
        assert_eq!(written, LNSD_DEFAULT_CONFIG.as_bytes());

        // A later --writeconfig leaves the file alone, edited or not.
        std::fs::write(&path, "[reticulum]\n").expect("edit");
        assert!(write_default_config(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[reticulum]\n"
        );
    }

    #[test]
    fn writeconfig_flag_parses() {
        assert!(parse(&["--writeconfig"]).writeconfig);
        assert!(!parse(&[]).writeconfig);
    }

    #[test]
    fn exampleconfig_flag_parses() {
        assert!(parse(&["--exampleconfig"]).exampleconfig);
        assert!(!parse(&[]).exampleconfig);
    }

    #[test]
    fn example_config_roundtrips_through_loader() {
        // The printed example must load through our own INI loader without
        // error and yield at least the Default AutoInterface.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config");
        std::fs::write(&path, example_config()).expect("write");

        let config = Config::load(&path).expect("example config must load");
        let auto = config
            .interfaces
            .get("Default Interface")
            .expect("Default Interface present");
        assert_eq!(auto.interface_type, "AutoInterface");
        assert!(auto.enabled);
    }
}
