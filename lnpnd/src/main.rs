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
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::FilePropagationStore;
use lnpnd::client::{ClientAction, ClientOptions};
use lnpnd::config::{
    default_config_dir, load_hash_file, loglevel_filter, parse_hash, RawConfig, EXAMPLE_CONFIG,
};
use lnpnd::engine::{Engine, EngineConfig, EngineEvent, DEFAULT_ANNOUNCE_INTERVAL_SECS};
use lnpnd::mailbox::MailboxConfig;

#[derive(Parser, Debug)]
#[command(
    name = "lnpnd",
    version,
    about = "LXMF propagation node daemon for Reticulum",
    long_about = "Runs an LXMF propagation node (a store-and-forward mailbox) \
                  on a running Reticulum shared instance.\n\n\
                  lnpnd attaches to a daemon that is already running (lnsd, or \
                  Python's rnsd) the way lnmsg and lblogd do; it does not start \
                  a Reticulum stack of its own. Clients (Sideband, meshchat, \
                  lxmd-based tools) configure the destination hash this prints \
                  at startup as their propagation node.\n\n\
                  Configuration lives in an lxmd-format config directory \
                  (--config; see --exampleconfig), and the remote-management \
                  verbs --status, --peers, --sync and --break drive lnpnd and \
                  lxmd nodes alike.\n\n\
                  Set LEVICULUM_EVENT_LOG=<path> for one structured line per \
                  accepted upload (PN_ACCEPT), rejected upload (PN_REJECT), \
                  mailbox request (PN_GET), eviction (PN_EVICT), peering \
                  change (PN_PEER), offer round (PN_OFFER), sync round \
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
    /// propagation_stamp_cost_target). 0 accepts uploads without
    /// proof-of-work; the Python reference never announces below 13,
    /// announcing lower is wire-legal and honoured.
    #[arg(long)]
    stamp_cost: Option<u8>,

    /// Peering cost announced to would-be peer nodes (config:
    /// peering_cost).
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
    /// message_storage_limit, which is in megabytes).
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

    /// Accept /offer only from static peers (config: from_static_only).
    #[arg(long)]
    from_static_only: bool,
}

/// Same derivation `lnmsg`, `lncp` and `lnstatus` use: the instance name
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
            Some(hash) => (ClientAction::Sync(hash), 10.0),
            None => {
                eprintln!("Invalid peer destination hash: {target}");
                return ExitCode::from(203);
            }
        }
    } else if let Some(target) = &args.unpeer {
        match parse_hash(target.trim()) {
            Some(hash) => (ClientAction::Unpeer(hash), 10.0),
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
            5.0,
        )
    };
    let timeout = Duration::from_secs_f64(args.timeout.unwrap_or(default_timeout).max(0.1));
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
    autopeer: bool,
    autopeer_maxdepth: u8,
    store_limit_kb: u64,
    transfer_limit_kb: u64,
    sync_limit_kb: u64,
    stamp_cost: u8,
    stamp_cost_flexibility: u8,
    peering_cost: u8,
    remote_peering_cost_max: u8,
    max_peers: usize,
    static_peers: Vec<[u8; 16]>,
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

fn resolve(args: &Args, file: &RawConfig) -> Result<Effective, lnpnd::config::ConfigError> {
    // `enable_node = no` would ask for an lxmd that is only a mailbox;
    // that job is lnmsg's. Refusing beats silently running the role the
    // config said not to run.
    if file.get_bool("propagation", "enable_node")? == Some(false) {
        return Err(lnpnd::config::ConfigError::BadValue {
            section: "propagation",
            key: "enable_node",
            value: "no (lnpnd is the propagation node; for a mailbox-only \
                    daemon use lxmd, or lnmsg for a client)"
                .to_string(),
        });
    }
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
        autopeer: args
            .autopeer
            .or(file.get_bool("propagation", "autopeer")?)
            .unwrap_or(true),
        autopeer_maxdepth: args
            .autopeer_maxdepth
            .or(file.get_u8("propagation", "autopeer_maxdepth")?)
            .unwrap_or(4),
        store_limit_kb: args.store_limit_kb.unwrap_or(
            file.get_u64("propagation", "message_storage_limit")?
                .map(|megabytes| megabytes * 1000)
                .unwrap_or(500_000),
        ),
        transfer_limit_kb: args.transfer_limit_kb.unwrap_or(
            // Both spellings, newer one wins, as in `apply_config`
            // (`lxmd.py:165-177`).
            file.get_u64("propagation", "propagation_message_max_accepted_size")?
                .or(file.get_u64("propagation", "propagation_transfer_max_accepted_size")?)
                .unwrap_or(4),
        ),
        sync_limit_kb: args.sync_limit_kb.unwrap_or(
            file.get_u64("propagation", "propagation_sync_max_accepted_size")?
                .unwrap_or(32),
        ),
        stamp_cost: args.stamp_cost.unwrap_or(
            file.get_u8("propagation", "propagation_stamp_cost_target")?
                .unwrap_or(0),
        ),
        stamp_cost_flexibility: file
            .get_u8("propagation", "propagation_stamp_cost_flexibility")?
            .unwrap_or(3),
        peering_cost: args
            .peering_cost
            .or(file.get_u8("propagation", "peering_cost")?)
            .unwrap_or(0),
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
        delivery_limit_kb: file
            .get_u64("lxmf", "delivery_transfer_max_accepted_size")?
            .unwrap_or(1000),
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
    let store = match FilePropagationStore::open(
        data_dir.join("messagestore"),
        effective.store_limit_kb * 1000,
    ) {
        Ok(store) => store,
        Err(error) => return failure(format!("message store: {error}")),
    };
    eprintln!(
        "lnpnd: message store holds {} message(s), {} of {} kB free",
        store.len(),
        store_free_kb(&store),
        effective.store_limit_kb
    );

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
        ..leviculum_lxmf::PropagationNodeConfig::default()
    };
    let peering = leviculum_lxmf::PeeringConfig {
        max_peers: effective.max_peers,
        autopeer: effective.autopeer,
        autopeer_maxdepth: effective.autopeer_maxdepth,
        peering_cost: effective.peering_cost,
        remote_peering_cost_max: effective.remote_peering_cost_max,
        max_inbound_syncs: effective.max_inbound_syncs,
        from_static_only: effective.from_static_only,
        static_peers: effective.static_peers.clone(),
    };
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
        store_limit_bytes: effective.store_limit_kb * 1000,
        delivery_limit_kb: effective.delivery_limit_kb,
    });

    let mut node = match ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(data_dir.join("storage"))
        .core_processor(engine)
        .build()
        .await
    {
        Ok(node) => node,
        Err(error) => {
            return failure(format!(
                "could not join the Reticulum shared instance named '{instance}'.\n  \
                 lnpnd talks to a daemon that is already running; start one with \
                 `lnsd` (or Python's `rnsd`), or name another instance with \
                 --instance / --rnsconfig.\n  The stack said: {error}"
            ))
        }
    };
    if let Err(error) = node.start().await {
        return failure(format!("node start: {error}"));
    }

    // Surface the engine's events on stderr (and the structured log); the
    // channel is drained on a blocking thread so the async runtime never
    // waits on it. Mailbox deliveries are written to disk and the
    // on_inbound hook runs here — deliberately off the core lock.
    let on_inbound = effective.on_inbound.clone();
    let printer = std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            report(event, &messages_dir, on_inbound.as_deref());
        }
    });

    // Run until interrupted; the store is durable at every append, so
    // shutdown has nothing to flush.
    if let Err(error) = tokio::signal::ctrl_c().await {
        return failure(format!("signal handling: {error}"));
    }
    eprintln!("lnpnd: shutting down");
    let _ = node.stop().await;
    drop(printer);
    ExitCode::SUCCESS
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

    #[test]
    fn the_public_node_config_resolves_key_for_key() {
        let file = RawConfig::parse(PUBLIC_NODE_CONFIG).expect("the live config shape parses");
        let args = Args::parse_from(["lnpnd"]);
        let effective = resolve(&args, &file).expect("resolves");

        // Minutes in the file, seconds in the daemon (`as_int(...)*60`,
        // `reference/LXMF/LXMF/Utilities/lxmd.py:154`).
        assert_eq!(effective.announce_interval_secs, 360 * 60);
        assert!(effective.autopeer);
        assert_eq!(effective.autopeer_maxdepth, 4);
        // Megabytes in the file, kilobytes in the daemon (`lxmd.py:158-159`).
        assert_eq!(effective.store_limit_kb, 1_000_000);
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

    /// `[propagation] announce_at_start` is not acted on — lnpnd announces
    /// the node shortly after start either way. It has to appear in the
    /// start-up warning, or an operator who set it to `no` gets an announce
    /// nobody told them about.
    #[test]
    fn propagation_announce_at_start_is_reported_as_inert() {
        let file = RawConfig::parse(PUBLIC_NODE_CONFIG).expect("parses");
        assert!(
            file.inert_keys()
                .iter()
                .any(|(section, key, _)| *section == "propagation" && *key == "announce_at_start"),
            "the key must be named in the start-up warning, not swallowed"
        );
        // The `[lxmf]` spelling IS acted on, and must not be warned about.
        let mailbox_only = RawConfig::parse("[lxmf]\nannounce_at_start = no\n").expect("parses");
        assert!(mailbox_only.inert_keys().is_empty());
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
