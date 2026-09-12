//! `lnmsg` binary: the non-interactive paths of the terminal LXMF messenger
//! described in `docs/src/concepts/lnmsg.md`.
//!
//! ```sh
//! echo "disk 91%" | lnmsg send <address> --title "backup"
//! lnmsg fetch
//! ```
//!
//! It attaches to a running `lnsd` or `rnsd` shared instance the way `lnomad`
//! does, runs one command through its own LXMF router and exits. A send that
//! worked prints nothing at all; errors go to stderr. It never starts a
//! Reticulum stack of its own, and it never claims a message was delivered.
//!
//! Exit codes extend `lnomad`'s convention (`lnomad/src/main.rs:174`, `:188`,
//! `:222`): 0 success, 1 operational failure, 2 argument error — plus 3,
//! "a propagation node holds the message", which `--via auto`'s fallback and
//! `--via propagated` return so a script can tell the two successes apart
//! (`lnmsg/src/send.rs`, `EXIT_PROPAGATED`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use leviculum_std::config::Config;

use lnmsg::engine::{attach, AttachConfig};
use lnmsg::fetch::{run_fetch, FetchOptions};
use lnmsg::outbox::{SendRequest, Via};
use lnmsg::seen::SeenStore;
use lnmsg::send::{
    fallback_reason, run_send, run_send_propagated, SendOptions, EXIT_FAILURE, EXIT_PROPAGATED,
    EXIT_USAGE,
};
use lnmsg::{address, body, display_name, events, identity};

/// Everything after attaching gets this many seconds by default: becoming
/// ready, learning a route to the destination, queueing, and getting the
/// message onto the network. A cron job needs to know when the command is
/// guaranteed to be gone more than it needs a slow path to succeed. In
/// `--via auto` the direct leg and the fallback leg each get one budget.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// `lnmsg fetch`'s default budget: a drain crosses the link twice for the
/// list round alone and the payload can be a mailbox-full, so it gets twice
/// the send budget before it is called stuck.
const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 60;

#[derive(Parser, Debug)]
#[command(
    name = "lnmsg",
    version = env!("LEVICULUM_VERSION"),
    about = "Send and fetch LXMF messages over Reticulum",
    long_about = "Send and fetch LXMF messages over a running Reticulum shared instance.\n\n\
                  lnmsg attaches to a daemon that is already running (lnsd, or \
                  Python's rnsd) the same way lnomad does. It does not start a \
                  Reticulum stack of its own, so a daemon has to be running.\n\n\
                  A send that worked prints nothing and exits 0, which means \
                  the message was queued cleanly -- not that it was delivered. \
                  Exit 3 means a propagation node holds the message for later \
                  collection. Failures explain themselves on stderr. Set \
                  LEVICULUM_EVENT_LOG=<path> for the full run as structured \
                  events, including the message id."
)]
struct Args {
    #[command(subcommand)]
    command: SubCommand,
}

#[derive(Subcommand, Debug)]
enum SubCommand {
    /// Queue one message for an LXMF address.
    Send(SendArgs),
    /// Drain the propagation node's mailbox and print what is new.
    Fetch(FetchArgs),
    /// Print this machine's own LXMF address.
    Address,
}

#[derive(clap::Args, Debug)]
struct SendArgs {
    /// Recipient's LXMF address: 32 hex characters, with or without the
    /// `lxmf@` prefix lnomad copies to the clipboard.
    address: String,

    /// Message body. Omitted, or given as `-`, the body is read from stdin —
    /// which is the form a script uses: `echo "disk 91%" | lnmsg send <addr>`.
    /// One trailing newline is removed.
    body: Option<String>,

    /// Message title (LXMF carries one; it may be empty).
    #[arg(long)]
    title: Option<String>,

    /// The name recipients see as the sender. Defaults to the account name of
    /// the user running lnmsg; `LNMSG_DISPLAY_NAME` sets it where there is no
    /// command line to edit, and this flag wins over it.
    #[arg(long, value_name = "NAME")]
    from: Option<String>,

    /// How the message travels. `auto` tries the direct delivery link first
    /// and falls back to a propagation node when none comes up; `direct` and
    /// `propagated` are the explicit choices.
    #[arg(long, value_enum, default_value_t = ViaArg::Auto)]
    via: ViaArg,

    /// The propagation node to use: 32 hex characters. Without it, the
    /// `propagation_node` key in lnmsg's config decides; without either, the
    /// most recently announced node heard this run.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default, like `lncp`).
    /// Only its instance name is read; lnmsg keeps its own state elsewhere.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds to spend, after connecting, on getting the message onto the
    /// network. This is not a wait for delivery: the command returns as soon
    /// as the stack has taken the message. In `--via auto` each leg gets
    /// this budget.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,
}

#[derive(clap::Args, Debug)]
struct FetchArgs {
    /// The propagation node to drain: 32 hex characters. Resolution order is
    /// the same as `send --pn`'s.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds for the whole drain: selecting the node, the link, the
    /// list/fetch/confirm round.
    #[arg(long, default_value_t = DEFAULT_FETCH_TIMEOUT_SECS)]
    timeout: u64,
}

/// The `--via` choice. `auto` is frontend orchestration over the seam's
/// [`Via`] values, so it has no engine-side mirror.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ViaArg {
    /// Direct link first; a propagation node when none comes up.
    Auto,
    /// Straight to the recipient over the mesh, never a mailbox.
    Direct,
    /// Through a propagation node's mailbox, never a direct attempt.
    Propagated,
}

fn usage(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("lnmsg: {message}");
    events::done(None, "usage", EXIT_USAGE);
    ExitCode::from(EXIT_USAGE)
}

fn failure(message: impl std::fmt::Display, outcome: &str) -> ExitCode {
    eprintln!("lnmsg: {message}");
    events::done(None, outcome, EXIT_FAILURE);
    ExitCode::from(EXIT_FAILURE)
}

/// The instance name decides the abstract socket (`\0rns/{name}`) the daemon
/// listens on, so it comes from the daemon's own config file. Same derivation
/// `lncp`, `lnstatus` and `leviculum-lxmf-node` use.
fn instance_name(config_dir: &std::path::Path) -> String {
    let config_file = config_dir.join("config");
    if config_file.exists() {
        if let Ok(config) = Config::load(&config_file) {
            return config.reticulum.instance_name;
        }
    }
    "default".to_string()
}

/// Resolve the propagation-node choice: the `--pn` flag, then lnmsg's own
/// config. `None` with `"announced"` means "let the engine take the most
/// recently announced node". The source word is what `LNMSG_PN` logs.
fn resolve_pn(
    flag: Option<&str>,
    home: &std::path::Path,
) -> Result<(Option<[u8; 16]>, &'static str), String> {
    if let Some(text) = flag {
        return match address::parse(text) {
            Ok(hash) => Ok((Some(hash), "flag")),
            Err(error) => Err(format!("--pn {}: {error}", text.trim())),
        };
    }
    match lnmsg::config::load(home) {
        Ok(config) => match config.propagation_node {
            Some(hash) => Ok((Some(hash), "config")),
            None => Ok((None, "announced")),
        },
        Err(error) => Err(error.to_string()),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    // `LEVICULUM_EVENT_LOG=<path>` turns the structured events into an
    // append-only file in the documented format; without it this is the plain
    // fmt subscriber and the events cost a filtered-out call. `warn` rather
    // than `info` as the default: a successful run says nothing on any stream,
    // so an `info`-level line on stderr would be the only thing a cron job
    // ever saw from a send that worked. Whoever wants the detail asks for the
    // event log, which is where the message id lives.
    leviculum_std::event_log::install_global_subscriber("warn");

    match args.command {
        SubCommand::Send(send) => run(send).await,
        SubCommand::Fetch(fetch) => run_fetch_command(fetch).await,
        SubCommand::Address => run_address(),
    }
}

/// `lnmsg address`: print our own LXMF address. No daemon involved — the
/// address is a pure function of the persistent identity, minted on first
/// use, so this is also how a fresh machine learns (and creates) the address
/// a correspondent should write down.
fn run_address() -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    match lnmsg::engine::delivery_address(&identity) {
        Ok(address) => {
            println!("{}", address::to_hex(&address));
            ExitCode::SUCCESS
        }
        Err(error) => failure(error, "no-identity"),
    }
}

async fn run(args: SendArgs) -> ExitCode {
    let destination = match address::parse(&args.address) {
        Ok(destination) => destination,
        Err(error) => return usage(format!("{}: {error}", args.address.trim())),
    };
    // Before the body is read: an unusable name is an argument error, and a
    // script piping into us should hear about it without first having its
    // stdin consumed.
    let display_name = match display_name::from_process(args.from.as_deref()) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    events::sender(&display_name.name, display_name.source.as_str());
    let body = match body::resolve(args.body.as_deref(), &mut std::io::stdin().lock()) {
        Ok(body) => body,
        Err(error) => return usage(error),
    };
    let title = args.title.unwrap_or_default().into_bytes();

    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    // Resolved before attaching for the same reason the display name is:
    // a bad --pn or a corrupt config line is an argument-shaped problem and
    // should not cost a daemon connection first.
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));

    let mut attached = match attach(AttachConfig {
        instance: instance.clone(),
        storage_dir: home.join("storage"),
        identity,
        display_name: display_name.name.into_bytes(),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return failure(error, "no-daemon"),
    };

    let options = SendOptions::new(instance, Duration::from_secs(args.timeout));
    let request = |via: Via| SendRequest {
        destination,
        title: title.clone(),
        body: body.clone(),
        via,
    };

    let code = match args.via {
        ViaArg::Direct => {
            let outcome = run_send(&mut attached.outbox, request(Via::Direct), &options).await;
            match outcome {
                Ok(queued) => {
                    events::done(Some(&queued.message_id), "queued", 0);
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("lnmsg: {error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
        ViaArg::Propagated => {
            let outcome = run_send_propagated(
                &mut attached.outbox,
                request(Via::Propagated),
                preferred_pn,
                pn_source,
                None,
                &options,
            )
            .await;
            match outcome {
                Ok(queued) => {
                    events::via("propagated", "requested");
                    events::done(Some(&queued.message_id), "propagated", EXIT_PROPAGATED);
                    ExitCode::from(EXIT_PROPAGATED)
                }
                Err(error) => {
                    eprintln!("lnmsg: {error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
        ViaArg::Auto => {
            // Leg one: the direct delivery link, forced (`Via::Link`). The
            // link either comes up or it does not; an opportunistic packet
            // would count as handed on with the peer unreachable, and the
            // fallback would never fire.
            let direct = run_send(&mut attached.outbox, request(Via::Link), &options).await;
            match direct {
                Ok(queued) => {
                    events::via("direct", "link-delivery");
                    events::done(Some(&queued.message_id), "queued", 0);
                    ExitCode::SUCCESS
                }
                Err(direct_error) => match fallback_reason(&direct_error) {
                    None => {
                        eprintln!("lnmsg: {direct_error}");
                        events::done(None, "failed", EXIT_FAILURE);
                        ExitCode::from(EXIT_FAILURE)
                    }
                    Some(reason) => {
                        events::via("propagated", reason);
                        eprintln!(
                            "lnmsg: no direct delivery to {} ({reason}); \
                             trying a propagation node",
                            address::to_hex(&destination)
                        );
                        // A stranded direct copy is cancelled so the peer
                        // cannot receive the message twice if it reappears
                        // while the upload runs.
                        let cancel = match &direct_error {
                            lnmsg::send::SendError::Stranded { message_id, .. } => {
                                Some(*message_id)
                            }
                            _ => None,
                        };
                        let fallback = run_send_propagated(
                            &mut attached.outbox,
                            request(Via::Propagated),
                            preferred_pn,
                            pn_source,
                            cancel,
                            &options,
                        )
                        .await;
                        match fallback {
                            Ok(queued) => {
                                events::done(
                                    Some(&queued.message_id),
                                    "propagated",
                                    EXIT_PROPAGATED,
                                );
                                ExitCode::from(EXIT_PROPAGATED)
                            }
                            Err(fallback_error) => {
                                eprintln!(
                                    "lnmsg: direct: {direct_error}\nlnmsg: propagated: \
                                     {fallback_error}"
                                );
                                events::done(None, "failed", EXIT_FAILURE);
                                ExitCode::from(EXIT_FAILURE)
                            }
                        }
                    }
                },
            }
        }
    };
    attached.stop().await;
    code
}

async fn run_fetch_command(args: FetchArgs) -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    let mut seen = match SeenStore::load(&home) {
        Ok(seen) => seen,
        Err(error) => return failure(format!("seen store: {error}"), "no-home"),
    };

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));

    // The display name travels in the delivery announce a fetch also sends;
    // there is no --from here because a fetch is not authored.
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };

    let mut attached = match attach(AttachConfig {
        instance: instance.clone(),
        storage_dir: home.join("storage"),
        identity,
        display_name: display_name.name.into_bytes(),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return failure(error, "no-daemon"),
    };

    let options = FetchOptions::new(instance, Duration::from_secs(args.timeout));
    // Deliberately NOT `stdout().lock()`: the engine's driver thread logs
    // through tracing, and with `RUST_LOG=debug` (how the conformance rig
    // runs every tool) the fmt subscriber writes to this same stdout. A lock
    // held across the whole fetch deadlocked the first engine tick against
    // it for the entire run. `run_fetch` renders each message into one
    // buffer and hands it over as a single write, so output stays whole
    // without anyone holding the stream.
    let mut stdout = std::io::stdout();
    let outcome = run_fetch(
        &mut attached.outbox,
        preferred_pn,
        pn_source,
        &options,
        &mut seen,
        &mut stdout,
    )
    .await;

    let code = match outcome {
        Ok(fetched) => {
            events::done(None, if fetched.new > 0 { "fetched" } else { "empty" }, 0);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("lnmsg: {error}");
            events::done(None, "failed", EXIT_FAILURE);
            ExitCode::from(EXIT_FAILURE)
        }
    };
    attached.stop().await;
    code
}
