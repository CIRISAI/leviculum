//! `lnomad` binary: a terminal browser for NomadNet micron pages.
//!
//! It connects to a running `lnsd`/`rnsd` shared instance, fetches the page at
//! the given URL, renders it to ANSI text, and (on a tty) enters an interactive
//! navigation loop. With `--print`, or when stdout is not a terminal, it fetches
//! and prints a single page and exits, for scripting and acceptance tests.

use std::io::{BufReader, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use leviculum_std::config::Config;
use lnomad::browser::{self, BrowserOptions};
use lnomad::cli::{resolve_args, Mode};
use lnomad::fetch::Session;
use lnomad::url::parse_url;

/// Fallback render width when no terminal size can be detected.
const FALLBACK_WIDTH: usize = 80;

#[derive(Parser, Debug)]
#[command(
    name = "lnomad",
    version = env!("CARGO_PKG_VERSION"),
    about = "Terminal browser for NomadNet micron pages"
)]
struct Args {
    /// Page URL: `<dest_hash>[:/page/x.mu[`f=v|...]]` (a bare hash opens the
    /// default page). In `--discover` mode it is instead an optional listen
    /// duration in seconds (equivalent to `--duration`).
    url: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default, like `lncp`).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Disable ANSI colour in the rendered output.
    #[arg(long)]
    no_color: bool,

    /// Render width in columns (default: the detected terminal width, else 80).
    #[arg(long)]
    width: Option<usize>,

    /// Per-request fetch timeout, in seconds.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Fetch, render and print the page once, then exit (non-interactive).
    #[arg(long)]
    print: bool,

    /// Discover NomadNet nodes from announces instead of fetching a page: listen
    /// for `nomadnetwork.node` announces and list the nodes seen.
    #[arg(long)]
    discover: bool,

    /// How long to listen in `--discover` mode, in seconds. Alternatively pass
    /// the seconds as the bare positional: `lnomad --discover [seconds]`.
    #[arg(long)]
    duration: Option<u64>,
}

/// Default listen duration in `--discover` mode, in seconds.
const DEFAULT_DISCOVER_DURATION: u64 = 30;

/// Detect the terminal width in columns, falling back to [`FALLBACK_WIDTH`].
fn detect_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .filter(|&w| w > 0)
        .unwrap_or(FALLBACK_WIDTH)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    // Resolve the positional: in page mode it is the URL, in --discover mode it
    // is an optional listen duration in seconds.
    let mode = match resolve_args(
        args.discover,
        args.url.as_deref(),
        args.duration,
        DEFAULT_DISCOVER_DURATION,
    ) {
        Ok(mode) => mode,
        Err(err) => {
            eprintln!("lnomad: {err}");
            return ExitCode::from(2);
        }
    };

    // In page mode, parse and validate the URL up front.
    let (target, discover_duration) = match &mode {
        Mode::Discover { duration } => (None, *duration),
        Mode::Page { url } => match parse_url(url, None) {
            Ok(target) => (Some(target), DEFAULT_DISCOVER_DURATION),
            Err(err) => {
                eprintln!("lnomad: {err}: {url}");
                return ExitCode::from(2);
            }
        },
    };

    let opts = BrowserOptions {
        width: args.width.unwrap_or_else(detect_width).max(1),
        no_color: args.no_color || !std::io::stdout().is_terminal(),
        timeout: Duration::from_secs(args.timeout),
    };

    // Connect to the shared instance: an explicit --instance overrides the
    // config file's instance name; otherwise resolve it like lncp does.
    let config_dir = args
        .config
        .clone()
        .unwrap_or_else(Config::default_config_dir);
    let connect = match &args.instance {
        Some(name) => Session::connect_to(name, config_dir.join("storage")).await,
        None => Session::connect(&config_dir).await,
    };
    let mut session = match connect {
        Ok(session) => session,
        Err(err) => {
            eprintln!("lnomad: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Print-once mode: also chosen automatically when stdout is not a tty, so a
    // piped/redirected invocation never blocks on the REPL.
    let interactive =
        !args.print && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let duration = Duration::from_secs(discover_duration);

    let code = if args.discover {
        let mut out = std::io::stdout();
        let result = if interactive {
            let stdin = std::io::stdin();
            let mut input = BufReader::new(stdin.lock());
            browser::discover_interactive(&mut input, &mut out, &mut session, duration, &opts).await
        } else {
            browser::discover_print(&mut out, &mut session, duration).await
        };
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("lnomad: {err}");
                ExitCode::FAILURE
            }
        }
    } else if let Some(target) = target {
        // Page mode: target was validated as present above.
        if interactive {
            let stdin = std::io::stdin();
            let mut input = BufReader::new(stdin.lock());
            let mut out = std::io::stdout();
            match browser::run(&mut input, &mut out, &mut session, target, &opts).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("lnomad: {err}");
                    ExitCode::FAILURE
                }
            }
        } else {
            let mut out = std::io::stdout();
            match browser::print_once(&mut out, &mut session, &target, &opts).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("lnomad: {err}");
                    ExitCode::FAILURE
                }
            }
        }
    } else {
        // Unreachable: page mode always validates a URL into `Some` above.
        eprintln!("lnomad: a page URL is required (or pass --discover)");
        ExitCode::from(2)
    };

    // Best-effort teardown; the exit code already reflects the fetch outcome.
    let _ = session.close().await;
    code
}
