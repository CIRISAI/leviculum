//! lnpnd — LXMF propagation node daemon. See `lib.rs` for why this is its
//! own binary (the shape `lxmd` has in the reference).
//!
//! The command line is `lxmd`'s (`reference/LXMF/LXMF/Utilities/lxmd.py:882-951`):
//! the same daemon flags, the same remote-management verbs
//! (`--status`, `--peers`, `--sync`, `--break`, `--remote`, `--identity`,
//! `--timeout`), the same `--exampleconfig`, and a config directory with
//! the same files. `lxmd --status --remote <hash>` and
//! `lnpnd --status --remote <hash>` are interchangeable against either
//! daemon — that interchange is proved in the conformance corpus.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::{DeliveryMethod, Message};
use leviculum_std::config::Config;
use leviculum_std::FilePropagationStore;
use lnpnd::client::{ClientAction, ClientOptions, DEFAULT_ACTION_TIMEOUT, DEFAULT_STATUS_TIMEOUT};
use lnpnd::config::{
    default_config_dir, load_hash_file, loglevel_filter, parse_hash, RawConfig, EXAMPLE_CONFIG,
};
use lnpnd::engine::{Engine, EngineConfig, EngineEvent, DEFAULT_ANNOUNCE_INTERVAL_SECS};
use lnpnd::mailbox::MailboxConfig;
use tokio::signal::unix::{signal, SignalKind};

/// Measured live heap, under the default-off `heap-accounting` feature
/// (550): the same counting shim lnsd installs (`leviculum-cli/src/lnsd.rs`),
/// for the same reason. It only counts and forwards to the allocator below
/// it, so the node runs the allocator it would run without it.
#[cfg(all(feature = "heap-accounting", not(feature = "alloc-mimalloc")))]
#[global_allocator]
static ALLOC: leviculum_std::heap_accounting::CountingAllocator =
    leviculum_std::heap_accounting::CountingAllocator;

/// mimalloc instead of the libc allocator, under the default
/// `alloc-mimalloc` feature (1592), as lnsd since 766: on miauhaus the
/// lnpnd on musl's mallocng grew about 70 MB an hour in anonymous mappings
/// no table owned, the retention lnsd showed there before it moved.
/// Counted, in the same shim, when `heap-accounting` is on too.
#[cfg(all(feature = "heap-accounting", feature = "alloc-mimalloc"))]
#[global_allocator]
static ALLOC: leviculum_std::heap_accounting::CountingOver<mimalloc::MiMalloc> =
    leviculum_std::heap_accounting::CountingOver(mimalloc::MiMalloc);

#[cfg(all(not(feature = "heap-accounting"), feature = "alloc-mimalloc"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `--version`: the crate version, the build id, `+heapacct` in an
/// instrumented build and the commit in parentheses, composed by build.rs the
/// way `leviculum-cli/build.rs` composes lnsd's.
const VERSION: &str = env!("LEVICULUM_VERSION");

#[derive(Parser, Debug)]
#[command(
    name = "lnpnd",
    version = VERSION,
    about = "LXMF propagation node daemon for Reticulum",
    long_about = "Runs an LXMF propagation node (a store-and-forward mailbox) \
                  on a running Reticulum shared instance.\n\n\
                  lnpnd attaches to a daemon that is already running (lnsd, or \
                  Python's rnsd) the way loquax and lblogd do; it does not start \
                  a Reticulum stack of its own. Clients (Sideband, meshchat, \
                  lxmd-based tools) configure the destination hash this prints \
                  at startup as their propagation node.\n\n\
                  Configuration lives in an lxmd-format config directory \
                  (--config; see --exampleconfig), and the remote-management \
                  verbs --status, --peers, --sync and --break drive lnpnd and \
                  lxmd nodes alike.\n\n\
                  Set LEVICULUM_EVENT_LOG=<path> for one structured line per \
                  accepted upload (PN_ACCEPT), rejected upload (PN_REJECT), \
                  mailbox request (PN_GET), eviction (PN_EVICT), departure \
                  with its reason (PN_DEPART), status query (PN_STATUS), \
                  peering change (PN_PEER), offer round (PN_OFFER), sync round \
                  (PN_SYNC) and own-mailbox delivery (PN_MAILBOX), plus a \
                  PN_STORE line every 8 minutes carrying store size against \
                  the limit -- which is also the log's liveness heartbeat. \
                  scripts/analyze-lnpnd.py summarises such a log."
)]
struct Args {
    /// Path to an alternative lnpnd config directory.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Path to an alternative Reticulum config directory (decides which
    /// shared instance to join).
    #[arg(long)]
    rnsconfig: Option<PathBuf>,

    /// Shared-instance name to connect to (overrides the Reticulum config
    /// file's).
    #[arg(long)]
    instance: Option<String>,

    /// Accepted for lxmd compatibility: lnpnd always runs the propagation
    /// node role.
    #[arg(short = 'p', long)]
    propagation_node: bool,

    /// Executable to run when a message is received in the daemon's own
    /// mailbox (overrides the config's `on_inbound`).
    #[arg(short = 'i', long, value_name = "PATH")]
    on_inbound: Option<String>,

    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[arg(short = 'q', long, action = clap::ArgAction::Count)]
    quiet: u8,

    /// Running as a service; log to <configdir>/logfile instead of the
    /// terminal.
    #[arg(short = 's', long)]
    service: bool,

    /// Display node status (remote management; local daemon unless
    /// --remote is given).
    #[arg(long)]
    status: bool,

    /// Display peered nodes (remote management).
    #[arg(long)]
    peers: bool,

    /// Request a sync with the specified peer (destination hash, hex).
    #[arg(long, value_name = "PEER")]
    sync: Option<String>,

    /// Break peering with the specified peer (destination hash, hex).
    #[arg(short = 'b', long = "break", value_name = "PEER")]
    unpeer: Option<String>,

    /// Timeout in seconds for query operations.
    #[arg(long)]
    timeout: Option<f64>,

    /// Remote propagation node destination hash (hex) for the
    /// remote-management verbs.
    #[arg(short = 'r', long, value_name = "HASH")]
    remote: Option<String>,

    /// Path to the identity used for remote requests (default: the config
    /// directory's identity).
    #[arg(long, value_name = "PATH")]
    identity: Option<PathBuf>,

    /// Print verbose configuration example to stdout and exit.
    #[arg(long)]
    exampleconfig: bool,

    /// Where the node keeps its message store and client state (default:
    /// <configdir>/storage).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Display name carried in the node announce's metadata (config:
    /// [propagation] node_name).
    #[arg(long)]
    name: Option<String>,

    /// Propagation stamp cost announced to clients (config:
    /// propagation_stamp_cost_target). Default 13, the Python
    /// reference's own floor; 0 accepts uploads without proof-of-work,
    /// announcing below 13 is wire-legal and honoured.
    #[arg(long)]
    stamp_cost: Option<u8>,

    /// Peering cost announced to would-be peer nodes (config:
    /// peering_cost). Default 1: a stock lxmd peer never syncs toward a
    /// node announcing 0.
    #[arg(long)]
    peering_cost: Option<u8>,

    /// Per-transfer limit in kilobytes (config:
    /// propagation_message_max_accepted_size).
    #[arg(long)]
    transfer_limit_kb: Option<u64>,

    /// Per-sync limit in kilobytes (config:
    /// propagation_sync_max_accepted_size).
    #[arg(long)]
    sync_limit_kb: Option<u64>,

    /// Message store capacity in kilobytes (config:
    /// message_storage_limit, which is in megabytes). Unset, the default
    /// of 500 MB is bounded by the host's RAM (lnpnd(1)).
    #[arg(long)]
    store_limit_kb: Option<u64>,

    /// Seconds between node announces (config: announce_interval, which
    /// is in minutes).
    #[arg(long)]
    announce_interval_secs: Option<u64>,

    /// Peer-table cap (config: max_peers).
    #[arg(long)]
    max_peers: Option<usize>,

    /// Static peers, comma-separated destination hashes (config:
    /// static_peers).
    #[arg(long, value_delimiter = ',')]
    static_peers: Vec<String>,

    /// Peer automatically on propagation announces (config: autopeer).
    #[arg(long, action = clap::ArgAction::Set)]
    autopeer: Option<bool>,

    /// Hop depth inside which announces create peers (config:
    /// autopeer_maxdepth).
    #[arg(long)]
    autopeer_maxdepth: Option<u8>,

    /// Highest remote peering cost we mine a key for (config:
    /// remote_peering_cost_max).
    #[arg(long)]
    remote_peering_cost_max: Option<u8>,

    /// Concurrent inbound sync transfers before /offer answers throttled
    /// (config: max_inbound_syncs).
    #[arg(long)]
    max_inbound_syncs: Option<usize>,

    /// Peer with static peers only: announce the node closed, and refuse
    /// offers and uploads from anyone else (config: from_static_only).
    #[arg(long)]
    from_static_only: bool,
}

/// Same derivation `loquax`, `lncp` and `lnstatus` use: the instance name
/// comes from the daemon's own config file unless overridden.
fn instance_name(config_dir: &Path) -> String {
    let config_file = config_dir.join("config");
    if config_file.exists() {
        if let Ok(config) = Config::load(&config_file) {
            return config.reticulum.instance_name;
        }
    }
    "default".to_string()
}

fn failure(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("lnpnd: {message}");
    ExitCode::FAILURE
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    if args.exampleconfig {
        print!("{EXAMPLE_CONFIG}");
        return ExitCode::SUCCESS;
    }

    let config_dir = args.config.clone().unwrap_or_else(default_config_dir);
    let rns_config_dir = args
        .rnsconfig
        .clone()
        .unwrap_or_else(Config::default_config_dir);
    let instance = args
        .instance
        .clone()
        .unwrap_or_else(|| instance_name(&rns_config_dir));

    if args.status || args.peers || args.sync.is_some() || args.unpeer.is_some() {
        return remote_command(&args, &config_dir, instance).await;
    }

    daemon(&args, &config_dir, instance).await
}

/// The remote-management verbs (`lxmd.py:908-943`): resolve the identity,
/// run the client, exit with the reference's codes.
async fn remote_command(args: &Args, config_dir: &Path, instance: String) -> ExitCode {
    // The client's log level, the reference's way (`_remote_init`,
    // `lxmd.py:877-878`): the config's loglevel — default 3 there, unlike
    // the daemon's 4 — shifted by -v/-q, RUST_LOG winning when set. Until
    // #427 only the daemon path installed a subscriber, so a client `-vvv`
    // was silence and a field timeout could not even show its own dial.
    // Stderr, so stdout stays the line-for-line lxmd output scripts parse.
    let file_loglevel = RawConfig::load(&config_dir.join("config"))
        .ok()
        .and_then(|file| file.get_u64("logging", "loglevel").ok().flatten())
        .unwrap_or(3);
    let level = (file_loglevel as i64 + args.verbose as i64 - args.quiet as i64).clamp(0, 7) as u64;
    leviculum_std::event_log::install_global_subscriber(loglevel_filter(level));

    let identity_path = match &args.identity {
        Some(path) => path.clone(),
        None => {
            // Without --identity the config directory must already hold
            // one (`_remote_init`, `lxmd.py:855-865`): minting a fresh
            // identity here would query with an address the remote has
            // never allowed.
            if !config_dir.is_dir() {
                eprintln!("Specified configuration directory does not exist, exiting now");
                return ExitCode::from(201);
            }
            let path = config_dir.join("identity");
            if !path.is_file() {
                eprintln!(
                    "Identity file not found in specified configuration directory, exiting now"
                );
                return ExitCode::from(202);
            }
            path
        }
    };
    if args.identity.is_some() && !identity_path.is_file() {
        eprintln!("Identity file not found in specified configuration directory, exiting now");
        return ExitCode::from(202);
    }
    // The file was proved to exist above, so this is a load: the client
    // verbs never mint. Querying with an address the remote has never
    // allowed would fail with `access denied` and no hint why.
    let identity = match lnpnd::identity::load_or_create(&identity_path) {
        Ok((identity, _)) => identity,
        Err(error) => {
            eprintln!("Could not load the Primary Identity from {error}");
            return ExitCode::from(4);
        }
    };
    let remote = match &args.remote {
        None => None,
        Some(raw) => match parse_hash(raw.trim()) {
            Some(hash) => Some(hash),
            None => {
                eprintln!("Invalid remote destination hash: {raw}");
                return ExitCode::from(203);
            }
        },
    };
    let (action, default_timeout) = if let Some(target) = &args.sync {
        match parse_hash(target.trim()) {
            Some(hash) => (ClientAction::Sync(hash), DEFAULT_ACTION_TIMEOUT),
            None => {
                eprintln!("Invalid peer destination hash: {target}");
                return ExitCode::from(203);
            }
        }
    } else if let Some(target) = &args.unpeer {
        match parse_hash(target.trim()) {
            Some(hash) => (ClientAction::Unpeer(hash), DEFAULT_ACTION_TIMEOUT),
            None => {
                eprintln!("Invalid peer destination hash: {target}");
                return ExitCode::from(203);
            }
        }
    } else {
        (
            ClientAction::Status {
                show_status: args.status,
                show_peers: args.peers,
            },
            DEFAULT_STATUS_TIMEOUT,
        )
    };
    let timeout = match args.timeout {
        Some(seconds) => Duration::from_secs_f64(seconds.max(0.1)),
        None => default_timeout,
    };
    // A per-invocation client state directory: the client learns paths
    // and identities but must not collide with a running daemon's store.
    let storage_dir = std::env::temp_dir().join(format!("lnpnd-client-{}", std::process::id()));
    let code = lnpnd::client::run(
        ClientOptions {
            instance,
            storage_dir: storage_dir.clone(),
            identity,
            remote,
            timeout,
        },
        action,
    )
    .await;
    let _ = std::fs::remove_dir_all(&storage_dir);
    ExitCode::from(code)
}

/// Everything the daemon reads from config + flags, resolved.
struct Effective {
    node_name: Option<String>,
    announce_interval_secs: u64,
    node_announce_at_start: bool,
    autopeer: bool,
    autopeer_maxdepth: u8,
    /// The limit the config or a flag set, `None` for the default.
    store_limit_kb: Option<u64>,
    transfer_limit_kb: u64,
    sync_limit_kb: u64,
    stamp_cost: u8,
    stamp_cost_flexibility: u8,
    peering_cost: u8,
    remote_peering_cost_max: u8,
    max_peers: usize,
    static_peers: Vec<[u8; 16]>,
    prioritised_destinations: Vec<[u8; 16]>,
    from_static_only: bool,
    max_inbound_syncs: usize,
    auth_required: bool,
    control_allowed: Vec<[u8; 16]>,
    display_name: String,
    mailbox_announce_at_start: bool,
    mailbox_announce_interval_secs: Option<u64>,
    mailbox_stamp_cost: u8,
    delivery_limit_kb: u64,
    on_inbound: Option<String>,
    loglevel: u64,
}

/// The peer table's configuration from the resolved settings.
fn peering_config(effective: &Effective) -> leviculum_lxmf::PeeringConfig {
    leviculum_lxmf::PeeringConfig {
        max_peers: effective.max_peers,
        autopeer: effective.autopeer,
        autopeer_maxdepth: effective.autopeer_maxdepth,
        peering_cost: effective.peering_cost,
        remote_peering_cost_max: effective.remote_peering_cost_max,
        max_inbound_syncs: effective.max_inbound_syncs,
        from_static_only: effective.from_static_only,
        static_peers: effective.static_peers.clone(),
    }
}

/// The store limit's line, and the warning when a configured limit's
/// index would take more than the RAM rule allows (order 1548).
fn log_store_limit(limit: &lnpnd::store_limit::StoreLimit) {
    use lnpnd::store_limit::megabytes;
    let ram_mb = limit.ram_bytes.map_or(0, megabytes);
    let index_mb = megabytes(limit.index_bytes());
    tracing::info!(
        event = "PN_STORE_LIMIT",
        bytes = limit.bytes,
        source = limit.source.as_str(),
        ram_mb = ram_mb,
        index_mb = index_mb,
    );
    if limit.exceeds_ram_rule() {
        tracing::warn!(
            "lnpnd: the message store limit of {} MB needs about {index_mb} MB of RAM \
             for its index when full, the host has {ram_mb} MB; a quarter of it \
             holds a store of {} MB",
            megabytes(limit.bytes),
            limit.ram_bound_bytes.map_or(0, megabytes),
        );
    }
}

fn resolve(args: &Args, file: &RawConfig) -> Result<Effective, lnpnd::config::ConfigError> {
    // `enable_node = no` would ask for an lxmd that is only a mailbox;
    // that job is loquax's. Refusing beats silently running the role the
    // config said not to run.
    if file.get_bool("propagation", "enable_node")? == Some(false) {
        return Err(lnpnd::config::ConfigError::BadValue {
            section: "propagation",
            key: "enable_node",
            value: "no (lnpnd is the propagation node; for a mailbox-only \
                    daemon use lxmd, or loquax for a client)"
                .to_string(),
        });
    }
    let limits =
        lnpnd::config::SizeLimits::resolve(file, args.transfer_limit_kb, args.sync_limit_kb)?;
    Ok(Effective {
        node_name: args
            .name
            .clone()
            .or_else(|| file.get("propagation", "node_name").map(str::to_string)),
        announce_interval_secs: args.announce_interval_secs.unwrap_or(
            file.get_u64("propagation", "announce_interval")?
                .map(|minutes| minutes * 60)
                .unwrap_or(DEFAULT_ANNOUNCE_INTERVAL_SECS),
        ),
        // On when absent, where `lxmd` reads an absent key as off
        // (`reference/LXMF/LXMF/Utilities/lxmd.py:123-126`): its generated
        // config writes `yes` (`:985`), so that is what nodes in the field
        // run, and a config without the key gets the second chance too.
        node_announce_at_start: file
            .get_bool("propagation", "announce_at_start")?
            .unwrap_or(true),
        autopeer: args
            .autopeer
            .or(file.get_bool("propagation", "autopeer")?)
            .unwrap_or(true),
        autopeer_maxdepth: args
            .autopeer_maxdepth
            .or(file.get_u8("propagation", "autopeer_maxdepth")?)
            .unwrap_or(lnpnd::config::DEFAULT_AUTOPEER_MAXDEPTH),
        // `None` leaves the limit to the RAM rule (`lnpnd::store_limit`).
        store_limit_kb: match args.store_limit_kb {
            Some(kb) => Some(kb),
            None => file
                .get_u64("propagation", "message_storage_limit")?
                .map(|megabytes| megabytes * 1000),
        },
        transfer_limit_kb: limits.transfer_kb,
        sync_limit_kb: limits.sync_kb,
        // Unset costs are the role's own defaults, 13 / 0 / 1 (Lead,
        // 2026-09-12; flexibility 0 since 1294), one source with the board's.
        stamp_cost: args.stamp_cost.unwrap_or(
            file.get_u8("propagation", "propagation_stamp_cost_target")?
                .unwrap_or(leviculum_lxmf::PropagationNodeConfig::default().stamp_cost),
        ),
        stamp_cost_flexibility: file
            .get_u8("propagation", "propagation_stamp_cost_flexibility")?
            .unwrap_or(leviculum_lxmf::PropagationNodeConfig::default().stamp_cost_flexibility),
        peering_cost: args
            .peering_cost
            .or(file.get_u8("propagation", "peering_cost")?)
            .unwrap_or(leviculum_lxmf::PropagationNodeConfig::default().peering_cost),
        remote_peering_cost_max: args
            .remote_peering_cost_max
            .or(file.get_u8("propagation", "remote_peering_cost_max")?)
            .unwrap_or(26),
        max_peers: args.max_peers.unwrap_or(
            file.get_u64("propagation", "max_peers")?
                .map(|value| value as usize)
                .unwrap_or(20),
        ),
        static_peers: if args.static_peers.is_empty() {
            file.get_hash_list("propagation", "static_peers")?
                .unwrap_or_default()
        } else {
            let mut hashes = Vec::new();
            for raw in &args.static_peers {
                let raw = raw.trim();
                if raw.is_empty() {
                    continue;
                }
                match parse_hash(raw) {
                    Some(hash) => hashes.push(hash),
                    None => {
                        return Err(lnpnd::config::ConfigError::BadValue {
                            section: "propagation",
                            key: "static_peers",
                            value: raw.to_string(),
                        })
                    }
                }
            }
            hashes
        },
        // lxmd reads the list and prioritises each valid hash
        // (`reference/LXMF/LXMF/Utilities/lxmd.py:214-217`, `:446-450`);
        // a bad hash is a config error here, as in `static_peers`.
        prioritised_destinations: file
            .get_hash_list("propagation", "prioritise_destinations")?
            .unwrap_or_default(),
        from_static_only: args.from_static_only
            || file
                .get_bool("propagation", "from_static_only")?
                .unwrap_or(false),
        max_inbound_syncs: args.max_inbound_syncs.unwrap_or(
            file.get_u64("propagation", "max_inbound_syncs")?
                .map(|value| (value as usize).max(1))
                .unwrap_or(3),
        ),
        auth_required: file
            .get_bool("propagation", "auth_required")?
            .unwrap_or(false),
        control_allowed: file
            .get_hash_list("propagation", "control_allowed")?
            .unwrap_or_default(),
        display_name: file
            .get("lxmf", "display_name")
            .unwrap_or("Anonymous Peer")
            .to_string(),
        mailbox_announce_at_start: file.get_bool("lxmf", "announce_at_start")?.unwrap_or(false),
        mailbox_announce_interval_secs: file
            .get_u64("lxmf", "announce_interval")?
            .map(|minutes| minutes * 60),
        // The reference clamps to ≥ 1 (`max(1, …)`, `lxmd.py:91`).
        mailbox_stamp_cost: file.get_u8("lxmf", "stamp_cost")?.unwrap_or(12).max(1),
        delivery_limit_kb: limits.delivery_kb,
        on_inbound: args
            .on_inbound
            .clone()
            .or_else(|| file.get("lxmf", "on_inbound").map(str::to_string)),
        loglevel: file.get_u64("logging", "loglevel")?.unwrap_or(4),
    })
}

async fn daemon(args: &Args, config_dir: &Path, instance: String) -> ExitCode {
    let config_file = config_dir.join("config");
    if !config_file.is_file() {
        if let Err(error) = std::fs::create_dir_all(config_dir) {
            return failure(format!("{}: {error}", config_dir.display()));
        }
        if let Err(error) = std::fs::write(&config_file, EXAMPLE_CONFIG) {
            return failure(format!("{}: {error}", config_file.display()));
        }
        eprintln!(
            "lnpnd: no config file found, default created at {}",
            config_file.display()
        );
    }
    let file = match RawConfig::load(&config_file) {
        Ok(file) => file,
        Err(error) => return failure(error),
    };
    let effective = match resolve(args, &file) {
        Ok(effective) => effective,
        Err(error) => return failure(error),
    };

    // Log level: the config's numeric level, shifted by -v/-q
    // (`program_setup`, `lxmd.py:361-365`); RUST_LOG wins when set.
    let level =
        (effective.loglevel as i64 + args.verbose as i64 - args.quiet as i64).clamp(0, 7) as u64;
    if args.service {
        leviculum_std::event_log::install_global_subscriber_to_file(
            loglevel_filter(level),
            &config_dir.join("logfile"),
        );
    } else {
        leviculum_std::event_log::install_global_subscriber(loglevel_filter(level));
    }
    // Said at startup, as lnsd does, so a SIGUSR1 dump can be traced to the
    // build that printed it.
    #[cfg(feature = "heap-accounting")]
    {
        tracing::info!(
            "Heap accounting: on, SIGUSR1 reports measured live bytes beside the estimate"
        );
        leviculum_std::heap_accounting::configure_sampler_from_env();
    }
    // Both ways, as lnsd's, so a field node's log names its allocator
    // whichever build it runs; build-deb.sh reads the mimalloc line off the
    // binary, so each arm is a whole literal.
    #[cfg(feature = "alloc-mimalloc")]
    tracing::info!("Allocator: mimalloc");
    #[cfg(not(feature = "alloc-mimalloc"))]
    tracing::info!("Allocator: system (libc)");

    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| config_dir.join("storage"));
    let messages_dir = data_dir.join("messages");
    if let Err(error) = std::fs::create_dir_all(&messages_dir) {
        return failure(format!("{}: {error}", messages_dir.display()));
    }

    for (section, key, why) in file.inert_keys() {
        tracing::warn!("lnpnd: config [{section}] {key} is accepted but not acted on: {why}");
    }

    // Before the node is built, so before anything can reach the air: a
    // first start that mints an address says so here, and a start that
    // finds one leaves it exactly as it was (`lnpnd::identity`).
    let identity = match lnpnd::identity::load_or_create(&config_dir.join("identity")) {
        Ok((identity, _)) => identity,
        Err(error) => return failure(error),
    };
    let store_dir = match lnpnd::message_store_dir(&data_dir) {
        Ok(store_dir) => store_dir,
        Err(error) => return failure(format!("message store: {error}")),
    };
    if let Some(old) = &store_dir.moved_from {
        eprintln!(
            "lnpnd: message store moved from {} to {}",
            old.display(),
            store_dir.path.display()
        );
    }
    // The default store limit follows the host's RAM, so a full store's
    // index cannot push the daemon into the OOM killer (order 1548).
    let store_limit = lnpnd::store_limit::resolve(
        effective.store_limit_kb.map(|kb| kb.saturating_mul(1000)),
        lnpnd::store_limit::host_ram_bytes(),
    );
    log_store_limit(&store_limit);
    let mut store = match FilePropagationStore::open(&store_dir.path, store_limit.bytes) {
        Ok(store) => store,
        Err(error) => return failure(format!("message store: {error}")),
    };
    // Once per store, before the node exists (`lnpnd::reprice_stored_values`).
    if !store.stamp_values_true() {
        let started = std::time::Instant::now();
        match lnpnd::reprice_stored_values(&mut store) {
            Ok(repriced) => eprintln!(
                "lnpnd: re-priced {repriced} message(s) stored at stamp value 0 in {} ms",
                started.elapsed().as_millis()
            ),
            // Not fatal: the store serves as before, its values as stored,
            // and without the marker the next start tries again.
            Err(error) => tracing::warn!("lnpnd: re-pricing the message store failed: {error}"),
        }
    }
    eprintln!(
        "lnpnd: message store holds {} message(s), {} of {} kB free",
        store.len(),
        store_free_kb(&store),
        store_limit.bytes / 1000
    );
    // What the start cost, and whether the index file spared it the walk
    // (order 1376: 130 s at 323 597 messages before it did).
    let opened = store.open_profile();
    eprintln!(
        "lnpnd: message store opened in {} ms, index file {}, {} from it, {} file(s) read",
        opened.total_us / 1000,
        opened.index_file.as_str(),
        opened.from_index_file,
        opened.files_read
    );
    // From here on the durable writes run on the store's own writer thread,
    // not in the hooks that hold the core lock (order 1081); the re-pricing
    // above ran before, inline, while nothing else was waiting.
    let store = match store.write_behind() {
        Ok(store) => store,
        Err(error) => return failure(format!("message store writer: {error}")),
    };

    let auth_allowed = if effective.auth_required {
        let allowed = load_hash_file(&config_dir.join("allowed"));
        if allowed.is_empty() {
            // The reference's warning, verbatim in spirit (`lxmd.py:437`).
            tracing::warn!(
                "lnpnd: client authentication is enabled, but no identity hashes could \
                 be loaded from {}. Nobody will be able to sync messages from this \
                 propagation node.",
                config_dir.join("allowed").display()
            );
        }
        Some(allowed)
    } else {
        None
    };
    let ignored = load_hash_file(&config_dir.join("ignored"));

    let node_config = leviculum_lxmf::PropagationNodeConfig {
        transfer_limit_kb: effective.transfer_limit_kb,
        sync_limit_kb: effective.sync_limit_kb,
        stamp_cost: effective.stamp_cost,
        stamp_cost_flexibility: effective.stamp_cost_flexibility,
        peering_cost: effective.peering_cost,
        name: effective.node_name.clone().map(String::into_bytes),
        prioritised_destinations: effective.prioritised_destinations.clone(),
        ..leviculum_lxmf::PropagationNodeConfig::default()
    };
    // Honoured, but said: a message stamped below what a stock peer takes
    // from us stays on this node until it expires (order 1294).
    if leviculum_lxmf::PropagationNode::new(
        leviculum_lxmf::MemoryPropagationStore::new(0),
        node_config.clone(),
    )
    .accepts_below_stock_peers()
    {
        tracing::warn!(
            "lnpnd: stamp cost {} minus flexibility {} accepts messages a stock \
             propagation node refuses (it demands {}); they stay here until they \
             expire",
            effective.stamp_cost,
            effective.stamp_cost_flexibility,
            leviculum_lxmf::STOCK_PEER_MIN_STAMP_VALUE
        );
    }
    let peering = peering_config(&effective);
    let peer_store = match leviculum_std::FilePeerStore::open(data_dir.join("peers")) {
        Ok(store) => store,
        Err(error) => return failure(format!("peer store: {error}")),
    };
    let (engine, events) = Engine::new(EngineConfig {
        identity,
        node_config,
        store,
        announce_interval_secs: effective.announce_interval_secs,
        announce_delay_secs: lnpnd::engine::ANNOUNCE_DELAY_SECS,
        announce_at_start: effective.node_announce_at_start,
        peering,
        peer_store: Box::new(peer_store),
        control_allowed: effective.control_allowed.clone(),
        auth_allowed,
        mailbox: Some(MailboxConfig {
            display_name: effective.display_name.clone().into_bytes(),
            stamp_cost: effective.mailbox_stamp_cost,
            announce_at_start: effective.mailbox_announce_at_start,
            announce_interval_secs: effective.mailbox_announce_interval_secs,
            delivery_limit_kb: effective.delivery_limit_kb,
            ignored,
        }),
        store_limit_bytes: store_limit.bytes,
        delivery_limit_kb: effective.delivery_limit_kb,
    });

    // The duplicate list's departed ids, beside the message store: a
    // message collected before a restart stays a duplicate after it.
    let engine = engine.with_departures_path(data_dir.join("departures"));
    // The role's own heap census (`PN_CENSUS`, order 1515): SIGUSR1 asks
    // for a line at the next tick, the printer marks what it took.
    let census = engine.census_handle();
    let printed = census.clone();

    // `node_builder` owns the driver configuration, including the reason
    // this daemon has no application event channel (Codeberg #419).
    let mut node = match lnpnd::node_builder(engine, data_dir.join("storage"))
        .connect_to_shared_instance(&instance)
        // The daemon's RPC key is under its config directory, not this
        // node's identity (1119).
        .shared_instance_config_dir(
            args.rnsconfig
                .clone()
                .unwrap_or_else(Config::default_config_dir),
        )
        .build()
        .await
    {
        Ok(node) => node,
        // Not the absent-daemon path: `build` only records the instance
        // name, it dials nothing. What fails here is the node itself —
        // storage that cannot be opened, a contradictory configuration.
        Err(error) => return failure(format!("node: {error}")),
    };
    // Waiting rather than exiting: at boot the shared instance is started
    // in the same transaction and has not bound its IPC socket yet, which
    // cost every boot and every fresh install one failed unit on lblogd
    // (Codeberg #311) and costs lnpnd the same. A daemon that never
    // appears still fails the start, after `lnpnd::DAEMON_WAIT`.
    if let Err(error) = lnpnd::start_waiting(&mut node, lnpnd::DAEMON_WAIT).await {
        return failure(format!(
            "could not join the Reticulum shared instance named '{instance}'.\n  \
             lnpnd talks to a daemon that is already running; start one with \
             `lnsd` (or Python's `rnsd`), or name another instance with \
             --instance / --rnsconfig.\n  The stack said: {error}"
        ));
    }

    // Surface the engine's events on stderr (and the structured log); the
    // channel is drained on a blocking thread so the async runtime never
    // waits on it. Mailbox deliveries are written to disk and the
    // on_inbound hook runs here — deliberately off the core lock.
    let on_inbound = effective.on_inbound.clone();
    let printer = std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            printed.event_taken();
            report(event, &messages_dir, on_inbound.as_deref());
        }
    });

    // Run until SIGINT or SIGTERM, and stop the node either way. The
    // message store is durable at every append and needs no flush, but the
    // node storage does: `known_destinations` is written on shutdown, so a
    // SIGTERM left to its default disposition (which is how systemd stops
    // and restarts the unit) forgot every destination learned in a run
    // shorter than the hourly crash-protection flush.
    //
    // SIGUSR1 prints the memory census, lnsd's signal for lnsd's dump and in
    // lnsd's shape (`leviculum_std::reticulum::compose_diagnostic_dump`), so
    // periculum's soak census reads either daemon with one parser (550). On
    // stderr, which is where every other line of this daemon goes. It also
    // asks the role for its `PN_CENSUS` line, which the engine writes at its
    // next tick, into the event log (order 1515).
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(sigterm) => sigterm,
        Err(error) => return failure(format!("signal handling: {error}")),
    };
    let mut sigusr1 = match signal(SignalKind::user_defined1()) {
        Ok(sigusr1) => sigusr1,
        Err(error) => return failure(format!("signal handling: {error}")),
    };
    let received = loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => match result {
                Ok(()) => break "SIGINT",
                Err(error) => return failure(format!("signal handling: {error}")),
            },
            _ = sigterm.recv() => break "SIGTERM",
            _ = sigusr1.recv() => {
                let (taken, _) = census.latest();
                census.request();
                // The engine takes the census at its next tick; the dump
                // waits for it so its total holds the role's tables of
                // now, and takes the last one if the tick is late.
                let deadline = tokio::time::Instant::now() + CENSUS_WAIT;
                while census.latest().0 == taken && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                eprint!("{}", census_dump(&node, census.dump_section()));
            }
        }
    };
    eprintln!("lnpnd: shutting down ({received})");
    let _ = node.stop().await;
    drop(printer);
    ExitCode::SUCCESS
}

/// How long a SIGUSR1 dump waits for the census it asked the engine for.
const CENSUS_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The SIGUSR1 dump: the node's census, the role's `PN_CENSUS` line under
/// it, one total over both, then the measured live line in a build that
/// can measure it. The total is what the `gap:` line subtracts as
/// `tracked`, so it holds the role's tables (order 1648): before, it held
/// the transport's alone and the gap's `unaccounted` counted the whole
/// store index. Written in one piece, because periculum tells a fresh dump
/// from the last by its `gap:` line and reads each section as the last
/// line of its kind.
fn census_dump(node: &leviculum_std::ReticulumNode, role: Option<(String, u64)>) -> String {
    let (own, own_bytes) = role.unwrap_or_else(|| {
        (
            "PN_CENSUS none yet: the engine has not registered\n".into(),
            0,
        )
    });
    #[allow(unused_mut)]
    let mut dump = leviculum_std::census::compose_multi_node_dump(
        &[("lnpnd".to_string(), node.diagnostic_dump())],
        &own,
        own_bytes,
    );
    #[cfg(feature = "heap-accounting")]
    dump.push_str(&leviculum_std::heap_accounting::measured_live_line(
        if cfg!(feature = "alloc-mimalloc") {
            "mimalloc"
        } else {
            "system"
        },
    ));
    dump
}

fn report(event: EngineEvent, messages_dir: &Path, on_inbound: Option<&str>) {
    match event {
        EngineEvent::Ready { destination_hash } => {
            eprintln!(
                "lnpnd: propagation node ready, destination {}",
                full_hex(&destination_hash)
            );
            eprintln!("lnpnd: clients set this hash as their propagation node");
        }
        EngineEvent::MailboxReady { delivery_hash } => {
            eprintln!(
                "lnpnd: mailbox ready, delivery {}",
                full_hex(&delivery_hash)
            );
        }
        EngineEvent::MailboxAnnounced => eprintln!("lnpnd: delivery announce sent"),
        EngineEvent::Inbound { message } => deliver_inbound(*message, messages_dir, on_inbound),
        EngineEvent::Broken { detail } => eprintln!("lnpnd: broken: {detail}"),
        EngineEvent::Announced => eprintln!("lnpnd: node announce sent"),
        EngineEvent::Accepted {
            transient_id,
            duplicate,
        } => {
            let hex = short_hex(&transient_id);
            if duplicate {
                eprintln!("lnpnd: duplicate upload {hex} re-proven");
            } else {
                eprintln!("lnpnd: stored message {hex}");
            }
        }
        EngineEvent::Rejected { detail } => eprintln!("lnpnd: rejected upload: {detail}"),
        EngineEvent::Served { form, count } => {
            eprintln!("lnpnd: served /get {form} ({count} message(s))")
        }
        EngineEvent::Evicted { count } => eprintln!("lnpnd: evicted {count} message(s)"),
        EngineEvent::Peer {
            action,
            destination_hash,
            reason,
        } => eprintln!(
            "lnpnd: peer {action} {} ({reason})",
            short_hex(&destination_hash)
        ),
        EngineEvent::Offer {
            dir,
            peer,
            offered,
            wanted,
        } => eprintln!(
            "lnpnd: offer {dir} peer {} offered {offered} wanted {wanted}",
            short_hex(&peer)
        ),
        EngineEvent::SyncDone {
            dir,
            peer,
            transferred,
            bytes,
            result,
        } => eprintln!(
            "lnpnd: sync {dir} peer {} transferred {transferred} ({bytes} B): {result}",
            short_hex(&peer)
        ),
        EngineEvent::KeyMined { peer, value } => eprintln!(
            "lnpnd: peering key mined for {} (value {value})",
            short_hex(&peer)
        ),
        EngineEvent::ControlServed { path, ok } => {
            eprintln!("lnpnd: control request {path} answered (ok={ok})")
        }
    }
}

/// One received mailbox message: write the reference's packed container
/// to the messages directory (`LXMessage.write_to_directory`,
/// `reference/LXMF/LXMF/LXMessage.py:674-698`: atomic tmp + rename,
/// fsync), then run the hook with the file path as its argument
/// (`lxmf_delivery`, `reference/LXMF/LXMF/Utilities/lxmd.py:302-306`).
fn deliver_inbound(message: Message, messages_dir: &Path, on_inbound: Option<&str>) {
    let file_name = full_hex(&message.message_id);
    let path = messages_dir.join(&file_name);
    let container = packed_container(&message);
    let tmp = messages_dir.join(format!("{file_name}.tmp.{}", std::process::id()));
    let written = std::fs::File::create(&tmp)
        .and_then(|mut fh| {
            fh.write_all(&container)?;
            fh.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        eprintln!(
            "lnpnd: could not write message file {}: {error}",
            path.display()
        );
        return;
    }
    eprintln!(
        "lnpnd: mailbox received message from {}, written to {}",
        short_hex(&message.source_hash),
        path.display()
    );
    let Some(command) = on_inbound else { return };
    // The reference builds `shlex.split(command + ' "<path>"')` and calls
    // it with stdout/stderr discarded (`lxmd.py:304-306`).
    let mut argv = shell_words(command);
    if argv.is_empty() {
        return;
    }
    argv.push(path.display().to_string());
    let program = argv.remove(0);
    match std::process::Command::new(&program)
        .args(&argv)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) if !status.success() => {
            eprintln!("lnpnd: on_inbound hook exited with {status}")
        }
        Ok(_) => {}
        Err(error) => eprintln!("lnpnd: on_inbound hook failed to run: {error}"),
    }
}

/// The reference's packed container for a message file
/// (`packed_container`, `reference/LXMF/LXMF/LXMessage.py:660-672`), with
/// the values an incoming delivered message carries: default state
/// (`GENERATING`, `LXMessage.py:147`), `transport_encrypted` true and the
/// EC description for a link/single delivery (`lxmf_delivery`,
/// `reference/LXMF/LXMF/LXMRouter.py:1885-1899`).
fn packed_container(message: &Message) -> Vec<u8> {
    let method = match message.method {
        DeliveryMethod::Opportunistic => 0x01u64,
        DeliveryMethod::Direct => 0x02,
        DeliveryMethod::Propagated => 0x03,
        DeliveryMethod::Paper => 0x05,
    };
    let encryption = match message.method {
        DeliveryMethod::Paper => "Unencrypted",
        _ => "Curve25519",
    };
    let mut out = Vec::new();
    msgpack::map(&mut out, 5);
    msgpack::string(&mut out, "state");
    msgpack::uint(&mut out, 0x00);
    msgpack::string(&mut out, "lxmf_bytes");
    msgpack::bin(&mut out, &message.pack());
    msgpack::string(&mut out, "transport_encrypted");
    msgpack::bool(&mut out, message.method != DeliveryMethod::Paper);
    msgpack::string(&mut out, "transport_encryption");
    msgpack::string(&mut out, encryption);
    msgpack::string(&mut out, "method");
    msgpack::uint(&mut out, method);
    out
}

/// A minimal `shlex.split`: whitespace-separated words with single and
/// double quoting. Enough for the hook commands the reference's example
/// shows (`on_inbound = rm`); a full shell is deliberately not offered.
fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_word = false;
    for character in command.chars() {
        match quote {
            Some(q) if character == q => quote = None,
            Some(_) => current.push(character),
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    has_word = true;
                }
                c if c.is_whitespace() => {
                    if has_word || !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                        has_word = false;
                    }
                }
                c => {
                    current.push(c);
                    has_word = true;
                }
            },
        }
    }
    if has_word || !current.is_empty() {
        words.push(current);
    }
    words
}

fn store_free_kb(store: &FilePropagationStore) -> u64 {
    use leviculum_lxmf::PropagationStore as _;
    store.free_space() / 1000
}

fn short_hex(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn full_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `lxmd`-shaped config a public propagation node actually carries,
    /// pinned key by key to the effective values it resolves to.
    ///
    /// This is the acceptance list for taking `leviculum.network`'s
    /// propagation node over from Python `lxmd`. Every key here is one an
    /// operator wrote on purpose; a value that silently resolves to
    /// something else is mail that silently stops flowing.
    const PUBLIC_NODE_CONFIG: &str = "\
[propagation]\n\
  enable_node = yes\n\
  announce_interval = 360\n\
  announce_at_start = yes\n\
  autopeer = yes\n\
  autopeer_maxdepth = 4\n\
  propagation_transfer_max_accepted_size = 256\n\
  message_storage_limit = 1000\n\
  static_peers = e17f833c4ddf8890dd3a79a6fea8161d\n\
  auth_required = no\n\
\n\
[lxmf]\n\
  display_name = leviculum.network\n\
  announce_at_start = no\n\
\n\
[logging]\n\
  loglevel = 4\n";

    /// No limit in the config and none on the command line leaves the
    /// limit to the RAM rule; a flag wins over the file (order 1548).
    #[test]
    fn an_unset_store_limit_is_left_to_the_ram_rule() {
        let empty = RawConfig::parse("[propagation]\n").expect("parses");
        let args = Args::parse_from(["lnpnd"]);
        assert_eq!(
            resolve(&args, &empty).expect("resolves").store_limit_kb,
            None
        );
        let file = RawConfig::parse(PUBLIC_NODE_CONFIG).expect("parses");
        let args = Args::parse_from(["lnpnd", "--store-limit-kb", "2000"]);
        assert_eq!(
            resolve(&args, &file).expect("resolves").store_limit_kb,
            Some(2000)
        );
    }

    #[test]
    fn the_public_node_config_resolves_key_for_key() {
        let file = RawConfig::parse(PUBLIC_NODE_CONFIG).expect("the live config shape parses");
        let args = Args::parse_from(["lnpnd"]);
        let effective = resolve(&args, &file).expect("resolves");

        // Minutes in the file, seconds in the daemon (`as_int(...)*60`,
        // `reference/LXMF/LXMF/Utilities/lxmd.py:154`).
        assert_eq!(effective.announce_interval_secs, 360 * 60);
        assert!(effective.node_announce_at_start);
        assert!(effective.autopeer);
        assert_eq!(effective.autopeer_maxdepth, 4);
        // Megabytes in the file, kilobytes in the daemon (`lxmd.py:158-159`).
        assert_eq!(effective.store_limit_kb, Some(1_000_000));
        // The older of the two spellings; kilobytes either way.
        assert_eq!(effective.transfer_limit_kb, 256);
        assert!(!effective.auth_required);
        assert_eq!(effective.display_name, "leviculum.network");
        assert!(
            !effective.mailbox_announce_at_start,
            "[lxmf] announce_at_start = no"
        );
        assert_eq!(effective.loglevel, 4);

        // A static peer dropped on the floor is mail that stops flowing
        // without anything saying so.
        assert_eq!(
            effective.static_peers,
            vec![[
                0xe1, 0x7f, 0x83, 0x3c, 0x4d, 0xdf, 0x88, 0x90, 0xdd, 0x3a, 0x79, 0xa6, 0xfe, 0xa8,
                0x16, 0x1d
            ]]
        );
    }

    /// A config that names no cost runs the Lead's decision of 2026-09-12,
    /// stamp cost 13 and peering cost 1, with flexibility 0 since order
    /// 1294 (13 and up, what a stock peer takes from us). Peering cost 0
    /// would leave every stock lxmd peer unable to sync toward us
    /// (`peering_key_ready` returns false on a falsy cost,
    /// `reference/LXMF/LXMF/LXMPeer.py:228`). The mailbox's own stamp cost
    /// is a different knob and keeps lxmd's 12.
    #[test]
    fn an_unset_cost_resolves_to_the_decided_default() {
        let file = RawConfig::parse(PUBLIC_NODE_CONFIG).expect("parses");
        let args = Args::parse_from(["lnpnd"]);
        let effective = resolve(&args, &file).expect("resolves");
        assert_eq!(
            [
                effective.stamp_cost,
                effective.stamp_cost_flexibility,
                effective.peering_cost
            ],
            [13, 0, 1]
        );
        assert_eq!(effective.mailbox_stamp_cost, 12);
    }

    /// A config that names no size limit runs lxmd's, 256 KB per message
    /// and 10 240 KB per sync (`reference/LXMF/LXMF/LXMRouter.py:55`,
    /// `:59`), the values the example config shows; 4 and 32 until 1294.
    #[test]
    fn an_unset_size_limit_resolves_to_the_references() {
        let file = RawConfig::parse("[propagation]\nenable_node = yes\n").expect("parses");
        let args = Args::parse_from(["lnpnd"]);
        let effective = resolve(&args, &file).expect("resolves");
        assert_eq!(
            (effective.transfer_limit_kb, effective.sync_limit_kb),
            (256, 10_240)
        );
    }

    /// `prioritise_destinations` reaches the role (order 1294): it was
    /// parsed, warned about and dropped.
    #[test]
    fn prioritise_destinations_is_honoured() {
        let file = RawConfig::parse(
            "[propagation]\nprioritise_destinations = \
             41d20c727598a3fbbdf9106133a3a0ed, d924b81822ca24e68e2effea99bcb8cf\n",
        )
        .expect("parses");
        let args = Args::parse_from(["lnpnd"]);
        let effective = resolve(&args, &file).expect("resolves");
        assert_eq!(effective.prioritised_destinations.len(), 2);
        assert_eq!(effective.prioritised_destinations[0][0], 0x41);
        assert!(file.inert_keys().is_empty());
    }

    /// A config that names no autopeer depth peers six hops deep, the depth
    /// lxmd's generated config carries (`lxmd.py:995`), not the router's
    /// code default of 4 (leviculum#442 D10).
    #[test]
    fn an_unset_autopeer_depth_resolves_to_six() {
        let file = RawConfig::parse("[propagation]\nenable_node = yes\n").expect("parses");
        let args = Args::parse_from(["lnpnd"]);
        let effective = resolve(&args, &file).expect("resolves");
        assert_eq!(effective.autopeer_maxdepth, 6);
        assert_eq!(lnpnd::config::DEFAULT_AUTOPEER_MAXDEPTH, 6);
    }

    /// The resolved default reaches the peer table: a stock node announcing
    /// from six hops is peered, one at seven is declined
    /// (leviculum#442 D10).
    #[test]
    fn an_unset_autopeer_depth_peers_at_six_hops_and_declines_at_seven() {
        use leviculum_lxmf::peering::{DeclineReason, PeerChange, PeerTable};
        use leviculum_lxmf::propagation::PropagationNodeAnnounce;

        let file = RawConfig::parse("[propagation]\nenable_node = yes\n").expect("parses");
        let args = Args::parse_from(["lnpnd"]);
        let effective = resolve(&args, &file).expect("resolves");
        let mut table = PeerTable::new(peering_config(&effective));
        let announce = PropagationNodeAnnounce {
            legacy_support: false,
            timebase: 1,
            enabled: true,
            transfer_limit_kb: 256,
            sync_limit_kb: 10_240,
            stamp_cost: 16,
            stamp_cost_flexibility: 3,
            peering_cost: 18,
            metadata: vec![],
        };
        assert_eq!(
            table.handle_announce([6; 16], &announce, Some(6), 100, false),
            PeerChange::Added
        );
        assert_eq!(
            table.handle_announce([7; 16], &announce, Some(7), 100, false),
            PeerChange::Declined(DeclineReason::TooDeep)
        );
    }

    /// The template lines show the value an unset key resolves to, so an
    /// operator who uncomments one changes nothing.
    #[test]
    fn the_template_shows_the_resolved_cost_defaults() {
        let template = lnpnd::config::EXAMPLE_CONFIG;
        for line in [
            "# propagation_stamp_cost_target = 13",
            "# propagation_stamp_cost_flexibility = 0",
            "# propagation_message_max_accepted_size = 256",
            "# propagation_sync_max_accepted_size = 10240",
            "# peering_cost = 1",
            "\nautopeer_maxdepth = 6\n",
        ] {
            assert!(template.contains(line), "template lacks `{line}`");
        }
    }

    /// `[propagation] announce_at_start` is acted on: `no` leaves the
    /// router's own start announce alone, as in `lxmd`, absent means on.
    /// Being honoured, it is no longer named in the start-up warning.
    #[test]
    fn propagation_announce_at_start_is_honoured() {
        let args = Args::parse_from(["lnpnd"]);
        let off = RawConfig::parse("[propagation]\nannounce_at_start = no\n").expect("parses");
        assert!(
            !resolve(&args, &off)
                .expect("resolves")
                .node_announce_at_start
        );
        assert!(off.inert_keys().is_empty());
        let absent = RawConfig::parse("[propagation]\n").expect("parses");
        assert!(
            resolve(&args, &absent)
                .expect("resolves")
                .node_announce_at_start
        );
    }

    #[test]
    fn shell_words_splits_like_shlex() {
        assert_eq!(shell_words("rm"), vec!["rm"]);
        assert_eq!(
            shell_words("/usr/local/bin/hook --flag 'a b'"),
            vec!["/usr/local/bin/hook", "--flag", "a b"]
        );
        assert_eq!(shell_words("  "), Vec::<String>::new());
        assert_eq!(shell_words("say \"\""), vec!["say", ""]);
    }
}
