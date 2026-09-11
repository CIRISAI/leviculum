//! lnpnd — LXMF propagation node daemon. See `lib.rs` for why this is its
//! own binary (the shape `lxmd` has in the reference).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use leviculum_std::config::Config;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::FilePropagationStore;
use lnpnd::engine::{Engine, EngineConfig, EngineEvent, DEFAULT_ANNOUNCE_INTERVAL_SECS};

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
                  Set LEVICULUM_EVENT_LOG=<path> for one structured line per \
                  accepted upload (PN_ACCEPT), per mailbox request (PN_GET) and \
                  per eviction (PN_EVICT)."
)]
struct Args {
    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default). Only its
    /// instance name is read; lnpnd keeps its own state in --data-dir.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Where the node keeps its identity and message store.
    #[arg(long, default_value_os_t = default_data_dir())]
    data_dir: PathBuf,

    /// Display name carried in the node announce's metadata.
    #[arg(long)]
    name: Option<String>,

    /// Propagation stamp cost announced to clients (announce field 5[0]).
    /// 0 accepts uploads without proof-of-work; the Python reference never
    /// announces below 13, announcing lower is wire-legal and honoured.
    #[arg(long, default_value_t = 0)]
    stamp_cost: u8,

    /// Peering cost announced to would-be peer nodes (announce field 5[2]).
    /// Peering itself lands in a later part; the cost is announced now so
    /// the announce is complete.
    #[arg(long, default_value_t = 0)]
    peering_cost: u8,

    /// Per-transfer limit in kilobytes (announce field 3).
    #[arg(long, default_value_t = 4)]
    transfer_limit_kb: u64,

    /// Per-sync limit in kilobytes (announce field 4).
    #[arg(long, default_value_t = 32)]
    sync_limit_kb: u64,

    /// Message store capacity in kilobytes. The default matches lxmd's
    /// `message_storage_limit` default of 500 MB.
    #[arg(long, default_value_t = 500_000)]
    store_limit_kb: u64,

    /// Seconds between node announces. The default matches lxmd's
    /// `announce_interval` default of 360 minutes.
    #[arg(long, default_value_t = DEFAULT_ANNOUNCE_INTERVAL_SECS)]
    announce_interval_secs: u64,
}

fn default_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LNPND_HOME") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(dir).join("lnpnd");
    }
    match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home).join(".local/share/lnpnd"),
        Err(_) => PathBuf::from(".lnpnd"),
    }
}

/// Same derivation `lnmsg`, `lncp` and `lnstatus` use: the instance name
/// comes from the daemon's own config file unless overridden.
fn instance_name(config_dir: &std::path::Path) -> String {
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
    leviculum_std::event_log::install_global_subscriber("info");

    let identity = match lnpnd::identity::load_or_create(&args.data_dir.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error),
    };
    let store = match FilePropagationStore::open(
        args.data_dir.join("messagestore"),
        args.store_limit_kb * 1000,
    ) {
        Ok(store) => store,
        Err(error) => return failure(format!("message store: {error}")),
    };
    eprintln!(
        "lnpnd: message store holds {} message(s), {} of {} kB free",
        store.len(),
        store_free_kb(&store),
        args.store_limit_kb
    );

    let node_config = leviculum_lxmf::PropagationNodeConfig {
        transfer_limit_kb: args.transfer_limit_kb,
        sync_limit_kb: args.sync_limit_kb,
        stamp_cost: args.stamp_cost,
        // The reference's flexibility default (PROPAGATION_COST_FLEX,
        // reference/LXMF/LXMF/LXMRouter.py:53).
        stamp_cost_flexibility: 3,
        peering_cost: args.peering_cost,
        name: args.name.map(String::into_bytes),
        ..leviculum_lxmf::PropagationNodeConfig::default()
    };
    let (engine, events) = Engine::new(EngineConfig {
        identity,
        node_config,
        store,
        announce_interval_secs: args.announce_interval_secs,
        announce_delay_secs: lnpnd::engine::ANNOUNCE_DELAY_SECS,
    });

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    let mut node = match ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(args.data_dir.join("storage"))
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
                 --instance / --config.\n  The stack said: {error}"
            ))
        }
    };
    if let Err(error) = node.start().await {
        return failure(format!("node start: {error}"));
    }

    // Surface the engine's readiness (and its address) on stderr; the
    // channel is drained on a blocking thread so the async runtime never
    // waits on it.
    let printer = std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            match event {
                EngineEvent::Ready { destination_hash } => {
                    let hex: String = destination_hash
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
                    eprintln!("lnpnd: propagation node ready, destination {hex}");
                    eprintln!("lnpnd: clients set this hash as their propagation node");
                }
                EngineEvent::Broken { detail } => eprintln!("lnpnd: broken: {detail}"),
                EngineEvent::Announced => eprintln!("lnpnd: node announce sent"),
                EngineEvent::Accepted {
                    transient_id,
                    duplicate,
                } => {
                    let hex: String = transient_id
                        .iter()
                        .take(8)
                        .map(|byte| format!("{byte:02x}"))
                        .collect();
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
            }
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

fn store_free_kb(store: &FilePropagationStore) -> u64 {
    use leviculum_lxmf::PropagationStore as _;
    store.free_space() / 1000
}
