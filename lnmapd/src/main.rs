//! lnmapd — the map store daemon. See `lib.rs` and
//! `docs/src/concepts/map-server.md` for what it serves and why.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use lnmapd::node::{MapNode, MapNodeConfig};
use lnmapd::serve::{router, CacheConfig, Served};

#[derive(Parser, Debug)]
#[command(
    name = "lnmapd",
    version,
    about = "Map store daemon: serves signed map pieces over HTTP and Reticulum",
    long_about = "Serves a map store directory (written by lnmapcut) on two sides \
                  at once: over HTTP under /index and /piece/<id>, and over the \
                  Reticulum mesh as the lev.maps destination, through a running \
                  shared instance (lnsd, or Python's rnsd).\n\n\
                  With --upstream it is a front-door cache instead: it serves what \
                  it holds and fetches what it lacks once from the upstream lnmapd, \
                  bounded by --cache-cap-mb."
)]
struct Args {
    /// The store directory (pieces plus signed index).
    #[arg(long)]
    store: PathBuf,

    /// HTTP listen address.
    #[arg(long, default_value = "127.0.0.1:8140")]
    http: String,

    /// Serve the store over Reticulum through this shared instance.
    /// Without it only HTTP is served (the front door on workorse puts
    /// its own node beside the cache when the mesh side moves there).
    #[arg(long)]
    instance: Option<String>,

    /// The server identity file (the one lnmapcut signs with); only
    /// with --instance.
    #[arg(long)]
    identity: Option<PathBuf>,

    /// Node storage directory; only with --instance.
    #[arg(long)]
    storage: Option<PathBuf>,

    /// Display name in the lev.maps announce.
    #[arg(long, default_value = "Leviculum map server")]
    display_name: String,

    /// Re-announce cadence, seconds.
    #[arg(long, default_value_t = 6 * 60 * 60)]
    announce_secs: u64,

    /// Front-door mode: the upstream lnmapd's HTTP base URL.
    #[arg(long)]
    upstream: Option<String>,

    /// Front-door piece cache bound, megabytes.
    #[arg(long, default_value_t = 4096)]
    cache_cap_mb: u64,

    /// Front-door index refresh age, seconds.
    #[arg(long, default_value_t = 300)]
    index_ttl_secs: u64,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("lnmapd: runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lnmapd: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let served = match &args.upstream {
        Some(upstream) => Served::front_door(
            args.store.clone(),
            CacheConfig {
                upstream: upstream.clone(),
                cap_bytes: args.cache_cap_mb * 1024 * 1024,
                index_ttl: Duration::from_secs(args.index_ttl_secs),
            },
        ),
        None => Served::origin(args.store.clone()),
    };

    let listener = tokio::net::TcpListener::bind(&args.http)
        .await
        .map_err(|e| format!("bind {}: {e}", args.http))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("local addr: {e}"))?;
    eprintln!("lnmapd: HTTP on {local}");

    let mesh = match (&args.instance, &args.upstream) {
        (Some(instance), None) => {
            let identity = args
                .identity
                .clone()
                .ok_or("--instance needs --identity (the index-signing identity)")?;
            let storage = args
                .storage
                .clone()
                .unwrap_or_else(|| args.store.join("node-storage"));
            let node = MapNode::start(MapNodeConfig {
                instance_name: instance.clone(),
                identity_path: identity,
                storage_dir: storage,
                store_dir: args.store.clone(),
                display_name: args.display_name.clone(),
                announce_interval: Duration::from_secs(args.announce_secs),
            })
            .await
            .map_err(|e| e.to_string())?;
            eprintln!(
                "lnmapd: lev.maps destination {}",
                hex::encode(node.destination_hash().as_bytes())
            );
            Some(tokio::spawn(node.run()))
        }
        (Some(_), Some(_)) => {
            return Err(
                "--instance with --upstream is not supported yet: the front door \
                        serves HTTP; the mesh side of the front door is K5 work"
                    .into(),
            );
        }
        (None, _) => None,
    };

    let http = axum::serve(listener, router(served).into_make_service());
    match mesh {
        Some(node_task) => {
            tokio::select! {
                served = http => served.map_err(|e| format!("http: {e}")),
                node = node_task => match node {
                    Ok(Ok(())) => Err("the daemon connection closed".into()),
                    Ok(Err(e)) => Err(format!("node: {e}")),
                    Err(e) => Err(format!("node task: {e}")),
                },
            }
        }
        None => http.await.map_err(|e| format!("http: {e}")),
    }
}
