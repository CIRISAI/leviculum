//! lnpath - Reticulum Path Management Utility
//!
//! Standalone binary for the path-query half of the client vocabulary,
//! compatible with Python's rnpath (RNS/Utilities/rnpath.py, 1.3.5):
//! query a path to a destination, wait for it to arrive, and drop one the
//! daemon holds. Connects to a running daemon (lnsd or rnsd) via shared
//! instance IPC.
//!
//! # The flag surface, and what is deliberately not here
//!
//! The reference tool is three tools in one trenchcoat: the path query
//! below, table and rate *views* (`-t`, `-r`, `-m`), and the blackhole
//! administration verbs (`-b`, `-B`, `-U`, `-p`), plus remote management
//! of another instance (`-R`, `-i`, `-W`). This binary implements the
//! query, the wait and the drop — the `path_query` verb a test harness
//! and a user reach for — and offers no flag for the rest. Offering a
//! flag that means something subtly different is the failure mode the
//! drop-in rule exists to prevent; an absent flag is a gap a caller
//! notices immediately. `lnstatus --tables` already exposes the path
//! table, which is the view half of `-t`.
//!
//! # Why `-w` is the whole point
//!
//! Periculum #20: `rnprobe` steps give up after roughly 10 s regardless
//! of the configured timeout, which turns an unrelated scenario red. The
//! wait window here is computed from `-w` and nothing else, and the
//! end-to-end test asserts it differentially (two runs, two windows).

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{ArgAction, Parser};
use tracing_subscriber::EnvFilter;

use leviculum_core::constants::PATH_REQUEST_TIMEOUT_MS;
use leviculum_std::config::Config;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::DestinationHash;

// Shared with lnprobe and lnstest; each binary uses its own subset.
#[allow(dead_code)]
mod client_fmt;
#[allow(dead_code)]
mod daemon_rpc;

use client_fmt::{hex_decode, parse_destination_hash, prettyhexrep, Spinner};

/// rnpath.py:491 — the reference default for `-w` is
/// `Transport.PATH_REQUEST_TIMEOUT`.
const DEFAULT_TIMEOUT: f64 = PATH_REQUEST_TIMEOUT_MS as f64 / 1000.0;

/// The line the reference overwrites its spinner with before the verdict
/// (rnpath.py:465, 476): carriage return, 55 spaces, carriage return.
const RESET_LINE: &str = "\r                                                       \r";

#[derive(Parser, Debug)]
#[command(
    name = "lnpath",
    version = env!("LEVICULUM_VERSION"),
    about = "Reticulum Path Management Utility"
)]
struct Args {
    /// Path to alternative Reticulum config directory
    #[arg(long)]
    config: Option<PathBuf>,

    /// Remove the path to a destination
    #[arg(short = 'd', long)]
    drop: bool,

    /// Timeout before giving up
    #[arg(short = 'w', value_name = "seconds")]
    timeout: Option<f64>,

    /// Increase verbosity
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,

    /// Hexadecimal hash of the destination
    destination: Option<String>,
}

/// Map -v to a tracing filter, like lnprobe's: the client default is
/// quiet, because stdout is a parsed surface and stderr is the operator's.
fn log_filter(verbose: u8) -> &'static str {
    match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    }
}

fn main() {
    let args = Args::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let code = runtime.block_on(run(args));
    std::process::exit(code);
}

async fn run(args: Args) -> i32 {
    // Diagnostics on stderr, stdout reserved for the verdict lines a
    // caller parses — the same split as lnprobe and lncp.
    tracing_subscriber::fmt()
        .compact()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(log_filter(args.verbose))),
        )
        .with_writer(std::io::stderr)
        .init();

    let Some(destination_hexhash) = args.destination.as_deref() else {
        // rnpath prints its help when nothing was asked of it
        // (rnpath.py:511-514).
        use clap::CommandFactory;
        println!();
        let _ = Args::command().print_help();
        println!();
        return 0;
    };
    let dest_hash = match parse_destination_hash(destination_hexhash) {
        Ok(hash) => hash,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };

    let config_dir = args
        .config
        .clone()
        .unwrap_or_else(Config::default_config_dir);
    let config = daemon_rpc::load_config(&config_dir);
    let instance_name = daemon_rpc::resolve_instance_name(None, config.as_ref());
    let authkey = daemon_rpc::resolve_authkey(&config_dir, config.as_ref())
        .ok()
        .map(|(key, _)| key);

    if args.drop {
        return drop_path(&instance_name, authkey.as_ref(), &dest_hash).await;
    }
    query_path(
        &instance_name,
        &daemon_rpc::resolve_storage_path(&config_dir, config.as_ref()),
        authkey.as_ref(),
        &dest_hash,
        args.timeout.unwrap_or(DEFAULT_TIMEOUT),
    )
    .await
}

/// `-d`: drop the path the *daemon* holds (rnpath.py:405-408).
///
/// Python reaches the daemon over the shared-instance RPC here
/// (`Reticulum.drop_path`, Reticulum.py:1561-1569) and so do we. Dropping
/// a copy inside this process instead would print the same success line
/// and change nothing: the routing table that matters outlives the client
/// by definition, and this process exits a moment later. No client node
/// is built for this path — the RPC is the whole operation.
async fn drop_path(
    instance_name: &str,
    authkey: Option<&[u8; 32]>,
    dest_hash: &DestinationHash,
) -> i32 {
    let pretty = prettyhexrep(dest_hash.as_bytes());
    let Some(authkey) = authkey else {
        eprintln!(
            "No transport identity under the config directory, so the daemon's \
             RPC cannot be authenticated. Is this the config directory the \
             daemon runs with?"
        );
        println!("Unable to drop path to {pretty}. Does it exist?");
        return 1;
    };
    match leviculum_std::rpc_drop_path(instance_name, authkey, dest_hash.as_bytes()).await {
        Ok(true) => {
            println!("Dropped path to {pretty}");
            0
        }
        Ok(false) => {
            println!("Unable to drop path to {pretty}. Does it exist?");
            1
        }
        Err(e) => {
            eprintln!("{e}");
            println!("Unable to drop path to {pretty}. Does it exist?");
            1
        }
    }
}

/// The default mode (rnpath.py:447-477): request a path if none is known,
/// wait out the `-w` window, then report hops, next hop and the interface
/// it leaves on.
async fn query_path(
    instance_name: &str,
    storage_path: &std::path::Path,
    authkey: Option<&[u8; 32]>,
    dest_hash: &DestinationHash,
    timeout: f64,
) -> i32 {
    let mut node = match build_client(instance_name, storage_path).await {
        Ok(node) => node,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let code = wait_and_report(&node, instance_name, authkey, dest_hash, timeout).await;
    let _ = node.stop().await;
    code
}

async fn wait_and_report(
    node: &ReticulumNode,
    instance_name: &str,
    authkey: Option<&[u8; 32]>,
    dest_hash: &DestinationHash,
    timeout: f64,
) -> i32 {
    if !node.has_path(dest_hash) {
        if let Err(e) = node.request_path(dest_hash).await {
            eprintln!("{e}");
            return 1;
        }
        print!(
            "Path to {} requested   ",
            prettyhexrep(dest_hash.as_bytes())
        );
        let _ = std::io::stdout().flush();
    }

    // The window is `-w` and nothing else. The deadline is taken after the
    // request goes out, like the reference (rnpath.py:454), so connecting
    // to the daemon is not charged against the caller's budget.
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    let mut spinner = Spinner::new();
    while !node.has_path(dest_hash) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        spinner.tick();
    }

    if !node.has_path(dest_hash) {
        println!("{RESET_LINE}Path not found");
        return 1;
    }

    // Hops are read locally; the next hop and its interface are the
    // daemon's answer, because only the daemon routes (rnpath.py:462-464).
    let hops = node.hops_to(dest_hash);
    let next_hop = next_hop(instance_name, authkey, dest_hash).await;
    let (Some(hops), Some(next_hop)) = (hops, next_hop) else {
        // Python only checks the next hop here; a path whose hop count is
        // unreadable is the same kind of half-answer and gets the same
        // line rather than a fabricated number (Python would print its
        // PATHFINDER_M sentinel, 128, as if it were a measurement).
        println!("{RESET_LINE}Error: Invalid path data returned");
        return 1;
    };
    let ms = if hops != 1 { "s" } else { "" };
    let interface = next_hop_if_name(instance_name, authkey, dest_hash)
        .await
        .unwrap_or_else(|| "None".to_string());
    println!(
        "\rPath found, destination {} is {} hop{} away via {} on {}",
        prettyhexrep(dest_hash.as_bytes()),
        hops,
        ms,
        prettyhexrep(&next_hop),
        interface
    );
    0
}

async fn build_client(
    instance_name: &str,
    storage_path: &std::path::Path,
) -> Result<ReticulumNode, String> {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(instance_name)
        // Same sharing rationale as lncp and lnprobe: a transportless
        // client writes no paths or announces to storage. The path is the
        // daemon's own, resolved from the config (Codeberg #241) — deriving
        // it from the config directory here left a second identity beside a
        // config that named an external disk.
        .storage_path(storage_path.to_path_buf())
        .build_sync()
        .map_err(|e| connect_error(instance_name, &e))?;
    node.start()
        .await
        .map_err(|e| connect_error(instance_name, &e))?;
    Ok(node)
}

fn connect_error(instance_name: &str, error: &dyn std::fmt::Display) -> String {
    format!(
        "Could not connect to a running Reticulum daemon on rns/{instance_name}.\n\
         Start lnsd or rnsd first.\nDetail: {error}"
    )
}

/// Python `reticulum.get_next_hop(dest)` — an RPC over the shared instance.
async fn next_hop(
    instance_name: &str,
    authkey: Option<&[u8; 32]>,
    dest_hash: &DestinationHash,
) -> Option<Vec<u8>> {
    let authkey = authkey?;
    let value = leviculum_std::rpc_query_hash_param(
        instance_name,
        authkey,
        "next_hop",
        "destination_hash",
        dest_hash.as_bytes(),
    )
    .await
    .ok()?;
    hex_decode(value.as_str()?).ok()
}

/// Python `reticulum.get_next_hop_if_name(dest)`. The daemon answers the
/// literal string `"None"` where it has no interface name, and Python
/// prints whatever came back, so the caller passes it through unchanged.
async fn next_hop_if_name(
    instance_name: &str,
    authkey: Option<&[u8; 32]>,
    dest_hash: &DestinationHash,
) -> Option<String> {
    let authkey = authkey?;
    let value = leviculum_std::rpc_query_hash_param(
        instance_name,
        authkey,
        "next_hop_if_name",
        "destination_hash",
        dest_hash.as_bytes(),
    )
    .await
    .ok()?;
    Some(value.as_str()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_window_is_the_reference_path_request_timeout() {
        // rnpath's default for -w is RNS.Transport.PATH_REQUEST_TIMEOUT = 15.
        assert_eq!(DEFAULT_TIMEOUT, 15.0);
    }

    #[test]
    fn the_timeout_flag_is_the_only_source_of_the_window() {
        // A window that silently mixes in a daemon-derived term is the #20
        // defect one layer down: the caller's number stops being the
        // number. Parsing -w must yield exactly what was passed, and its
        // absence exactly the reference default.
        let args = Args::parse_from(["lnpath", "-w", "2.5", "00112233445566778899aabbccddeeff"]);
        assert_eq!(args.timeout, Some(2.5));
        let args = Args::parse_from(["lnpath", "00112233445566778899aabbccddeeff"]);
        assert_eq!(args.timeout.unwrap_or(DEFAULT_TIMEOUT), 15.0);
    }

    #[test]
    fn drop_and_query_are_distinguished_by_the_reference_flag() {
        let args = Args::parse_from(["lnpath", "-d", "00112233445566778899aabbccddeeff"]);
        assert!(args.drop);
        let args = Args::parse_from(["lnpath", "00112233445566778899aabbccddeeff"]);
        assert!(!args.drop);
    }

    #[test]
    fn the_reset_line_covers_the_line_it_clears() {
        // The verdict overwrites the request line, whose width is fixed:
        // a 16-byte hash renders at a known length and the spinner
        // backspaces over itself, so the line never grows past its first
        // form. 55 spaces is exactly that width in the reference, and a
        // reset narrower than the line leaves debris on the terminal.
        let request_line = format!("Path to {} requested   ", prettyhexrep(&[0xaa; 16]));
        let spaces = RESET_LINE.trim_matches('\r').len();
        assert_eq!(request_line.chars().count(), 55);
        assert_eq!(spaces, 55);
    }
}
