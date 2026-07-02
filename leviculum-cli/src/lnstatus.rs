//! `lnstatus` — native Rust drop-in for Python `rnstatus` (Codeberg #86).
//!
//! Stage 1 implements LOCAL mode: it drives a running `lnsd` (or Python `rnsd`)
//! over the shared-instance RPC (`interface_stats`, and `link_count` for `-l`),
//! then renders the exact rnstatus per-interface layout. Remote management
//! (`-R/-i/-w`) and discovered interfaces (`-d/-D`) are deferred (Stage 3 / #32)
//! and print a clear "not supported yet" notice.
//!
//! Output parity is the point: the same `interface_stats` dict fed to this
//! renderer and to Python rnstatus yields byte-identical output, so a
//! `lnstatus | diff rnstatus` against the same daemon passes. See
//! `lnstatus_render` for the format port and the golden-output tests.

use std::path::{Path, PathBuf};

use clap::Parser;

use leviculum_std::config::Config;

mod lnstatus_render;
use lnstatus_render::StatusOptions;

/// Reticulum Network Stack Status (drop-in for rnstatus).
#[derive(Parser, Debug)]
#[command(
    name = "lnstatus",
    version = env!("LEVICULUM_VERSION"),
    about = "Reticulum Network Stack Status"
)]
struct Args {
    /// path to alternative Reticulum config directory
    #[arg(long)]
    config: Option<PathBuf>,

    /// show all interfaces
    #[arg(short = 'a', long = "all", default_value_t = false)]
    all: bool,

    /// show announce stats
    #[arg(short = 'A', long = "announce-stats", default_value_t = false)]
    announce_stats: bool,

    /// show path request stats
    #[arg(short = 'P', long = "pr-stats", default_value_t = false)]
    pr_stats: bool,

    /// show link stats
    #[arg(short = 'l', long = "link-stats", default_value_t = false)]
    link_stats: bool,

    /// only show interfaces with active bursts
    #[arg(short = 'B', long = "burst", default_value_t = false)]
    burst: bool,

    /// display traffic totals
    #[arg(short = 't', long = "totals", default_value_t = false)]
    totals: bool,

    /// sort interfaces by [rate, traffic, rx, tx, rxs, txs, announces, arx, atx, prx, ptx, held]
    #[arg(short = 's', long = "sort")]
    sort: Option<String>,

    /// reverse sorting
    #[arg(short = 'r', long = "reverse", default_value_t = false)]
    reverse: bool,

    /// output in JSON format
    #[arg(short = 'j', long = "json", default_value_t = false)]
    json: bool,

    /// transport identity hash of remote instance to get status from (DEFERRED: #86 Stage 3)
    #[arg(short = 'R')]
    remote: Option<String>,

    /// path to identity used for remote management (DEFERRED: #86 Stage 3)
    #[arg(short = 'i')]
    identity: Option<PathBuf>,

    /// timeout before giving up on remote queries (DEFERRED: #86 Stage 3)
    #[arg(short = 'w')]
    timeout: Option<f64>,

    /// list discovered interfaces (DEFERRED: depends on #32)
    #[arg(short = 'd', long = "discovered", default_value_t = false)]
    discovered: bool,

    /// show details and config entries for discovered interfaces (DEFERRED: depends on #32)
    #[arg(short = 'D', default_value_t = false)]
    discovered_details: bool,

    /// continuously monitor status
    #[arg(short = 'm', long = "monitor", default_value_t = false)]
    monitor: bool,

    /// refresh interval for monitor mode (default: 1)
    #[arg(short = 'I', long = "monitor-interval", default_value_t = 1.0)]
    monitor_interval: f64,

    /// verbose logging (repeatable)
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,

    /// only display interfaces with names including filter
    filter: Option<String>,

    /// shared-instance name to query (default: from config, else "default")
    #[arg(long)]
    instance_name: Option<String>,
}

impl Args {
    fn status_options(&self) -> StatusOptions {
        StatusOptions {
            dispall: self.all,
            astats: self.announce_stats,
            pstats: self.pr_stats,
            lstats: self.link_stats,
            burst_filter: self.burst,
            totals: self.totals,
            sort: self.sort.clone(),
            reverse: self.reverse,
            name_filter: self.filter.clone(),
        }
    }
}

/// Resolve the daemon's RPC authkey: `SHA256(storage/transport_identity)`.
/// Mirrors the diag path (`leviculum-cli/src/diag.rs:303`).
fn resolve_authkey(config_dir: &Path, config: Option<&Config>) -> Result<[u8; 32], String> {
    let mut candidates: Vec<PathBuf> = vec![config_dir.join("storage").join("transport_identity")];
    if let Some(sp) = config.and_then(|c| c.reticulum.storage_path.as_ref()) {
        candidates.push(sp.join("transport_identity"));
    }
    let mut errors = Vec::new();
    for path in &candidates {
        match std::fs::read(path) {
            Ok(bytes) if bytes.len() == 64 => {
                use sha2::Digest;
                let digest = sha2::Sha256::digest(&bytes);
                let mut key = [0u8; 32];
                key.copy_from_slice(&digest);
                return Ok(key);
            }
            Ok(bytes) => errors.push(format!(
                "{}: unexpected size {} (expected 64)",
                path.display(),
                bytes.len()
            )),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    Err(errors.join("; "))
}

/// Fetch `interface_stats` (and `link_count` when `-l`) from the daemon.
async fn fetch_status(
    instance_name: &str,
    authkey: &[u8; 32],
    want_link_count: bool,
) -> Result<(serde_json::Value, Option<i64>), String> {
    let stats = leviculum_std::rpc_query(instance_name, authkey, "interface_stats")
        .await
        .map_err(|e| e.to_string())?;

    let link_count = if want_link_count {
        // rnstatus tolerates link_count failing (rnstatus.py:337-338); mirror that.
        match leviculum_std::rpc_query(instance_name, authkey, "link_count").await {
            Ok(v) => v
                .as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok())),
            Err(_) => None,
        }
    } else {
        None
    };
    Ok((stats, link_count))
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // --- Deferred features: clear notices, no silent no-op. ---
    if args.remote.is_some() || args.identity.is_some() {
        eprintln!(
            "lnstatus: remote management (-R/-i/-w) is not supported yet \
             (Codeberg #86 Stage 3); only local shared-instance status is available."
        );
        std::process::exit(2);
    }
    if args.discovered || args.discovered_details {
        eprintln!(
            "lnstatus: discovered interfaces (-d/-D) are not supported yet \
             (depends on Codeberg #32)."
        );
        std::process::exit(2);
    }

    // --- Resolve config dir / instance name / authkey. ---
    let config_dir = args
        .config
        .clone()
        .unwrap_or_else(Config::default_config_dir);
    let config_file = config_dir.join("config");
    let loaded_config: Option<Config> = if config_file.exists() {
        Config::load(&config_file).ok()
    } else {
        None
    };
    let instance_name = args
        .instance_name
        .clone()
        .or_else(|| {
            loaded_config
                .as_ref()
                .map(|c| c.reticulum.instance_name.clone())
        })
        .unwrap_or_else(|| "default".to_string());

    let authkey = match resolve_authkey(&config_dir, loaded_config.as_ref()) {
        Ok(k) => k,
        Err(msg) => {
            eprintln!("No shared RNS instance available to get status from");
            eprintln!("(cannot derive RPC authkey: {msg})");
            std::process::exit(1);
        }
    };

    let opts = args.status_options();

    if args.monitor {
        run_monitor(&instance_name, &authkey, &args, &opts).await;
        return;
    }

    match fetch_status(&instance_name, &authkey, args.link_stats).await {
        Ok((stats, link_count)) => {
            if args.json {
                println!("{}", lnstatus_render::render_json(&stats));
            } else {
                print!(
                    "{}",
                    lnstatus_render::render_status(&stats, link_count, &opts)
                );
            }
        }
        Err(e) => {
            eprintln!("Could not get RNS status");
            eprintln!("({e})");
            std::process::exit(2);
        }
    }
}

/// `-m/--monitor`: clear the screen and re-render on each interval. The redraw
/// uses the same single-shot render as the one-off path (rnstatus buffers its
/// output then clears + prints; this matches that behaviour).
async fn run_monitor(instance_name: &str, authkey: &[u8; 32], args: &Args, opts: &StatusOptions) {
    let interval = args.monitor_interval.max(0.2);
    loop {
        let rendered = match fetch_status(instance_name, authkey, args.link_stats).await {
            Ok((stats, link_count)) => {
                if args.json {
                    lnstatus_render::render_json(&stats) + "\n"
                } else {
                    lnstatus_render::render_status(&stats, link_count, opts)
                }
            }
            Err(e) => format!("Could not get RNS status\n({e})\n"),
        };
        // ANSI: cursor home + clear screen (rnstatus.py:738).
        print!("\x1b[H\x1b[2J{rendered}");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        tokio::time::sleep(std::time::Duration::from_secs_f64(interval)).await;
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Args {
        let mut v = vec!["lnstatus"];
        v.extend_from_slice(args);
        Args::try_parse_from(v).expect("parse ok")
    }

    #[test]
    fn defaults_are_all_false() {
        let a = parse(&[]);
        let o = a.status_options();
        assert!(!o.dispall && !o.astats && !o.pstats && !o.lstats);
        assert!(!o.burst_filter && !o.totals && !o.reverse);
        assert!(o.sort.is_none() && o.name_filter.is_none());
        assert!(!a.json && !a.monitor && !a.discovered);
        assert_eq!(a.monitor_interval, 1.0);
    }

    #[test]
    fn short_flags_map_to_options() {
        let o = parse(&["-a"]).status_options();
        assert!(o.dispall);
        assert!(parse(&["-A"]).status_options().astats);
        assert!(parse(&["-P"]).status_options().pstats);
        assert!(parse(&["-l"]).status_options().lstats);
        assert!(parse(&["-B"]).status_options().burst_filter);
        assert!(parse(&["-t"]).status_options().totals);
        assert!(parse(&["-r"]).status_options().reverse);
        assert!(parse(&["-j"]).json);
        assert!(parse(&["-m"]).monitor);
        assert!(parse(&["-d"]).discovered);
        assert!(parse(&["-D"]).discovered_details);
    }

    #[test]
    fn long_flags_map_to_options() {
        assert!(parse(&["--all"]).status_options().dispall);
        assert!(parse(&["--announce-stats"]).status_options().astats);
        assert!(parse(&["--pr-stats"]).status_options().pstats);
        assert!(parse(&["--link-stats"]).status_options().lstats);
        assert!(parse(&["--burst"]).status_options().burst_filter);
        assert!(parse(&["--totals"]).status_options().totals);
        assert!(parse(&["--reverse"]).status_options().reverse);
        assert!(parse(&["--json"]).json);
        assert!(parse(&["--monitor"]).monitor);
        assert!(parse(&["--discovered"]).discovered);
    }

    #[test]
    fn sort_and_reverse() {
        let a = parse(&["-s", "rate", "-r"]);
        let o = a.status_options();
        assert_eq!(o.sort.as_deref(), Some("rate"));
        assert!(o.reverse);
        assert_eq!(
            parse(&["--sort", "traffic"])
                .status_options()
                .sort
                .as_deref(),
            Some("traffic")
        );
    }

    #[test]
    fn positional_filter_and_combo() {
        let a = parse(&["-A", "-P", "eth0"]);
        let o = a.status_options();
        assert!(o.astats && o.pstats);
        assert_eq!(o.name_filter.as_deref(), Some("eth0"));
    }

    #[test]
    fn monitor_interval_parses_float() {
        let a = parse(&["-m", "-I", "2.5"]);
        assert!(a.monitor);
        assert_eq!(a.monitor_interval, 2.5);
    }

    #[test]
    fn remote_and_config_options() {
        let a = parse(&["-R", "abcdef", "-i", "/tmp/id", "-w", "3.0"]);
        assert_eq!(a.remote.as_deref(), Some("abcdef"));
        assert_eq!(a.identity.as_deref(), Some(std::path::Path::new("/tmp/id")));
        assert_eq!(a.timeout, Some(3.0));
        assert_eq!(
            parse(&["--config", "/etc/reticulum"]).config.as_deref(),
            Some(std::path::Path::new("/etc/reticulum"))
        );
        assert_eq!(
            parse(&["--instance-name", "foo"]).instance_name.as_deref(),
            Some("foo")
        );
    }

    #[test]
    fn verbose_counts() {
        assert_eq!(parse(&["-v"]).verbose, 1);
        assert_eq!(parse(&["-vvv"]).verbose, 3);
        assert_eq!(parse(&[]).verbose, 0);
    }

    #[test]
    fn bad_input_errors() {
        // Non-numeric -w / -I, and unknown flag, must be rejected.
        assert!(Args::try_parse_from(["lnstatus", "-w", "notanumber"]).is_err());
        assert!(Args::try_parse_from(["lnstatus", "-I", "abc"]).is_err());
        assert!(Args::try_parse_from(["lnstatus", "--nope"]).is_err());
        // Too many positionals (filter takes exactly one).
        assert!(Args::try_parse_from(["lnstatus", "a", "b"]).is_err());
    }
}
