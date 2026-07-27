//! `lblogd` binary: serve the blog over Reticulum and the clearnet.
//!
//! Serve mode starts the NomadNet page node and the HTTP/HTTPS web server
//! concurrently and runs until either fails; both are daemons, so any return
//! is an error and the process exits non zero. Both read their content from
//! one shared snapshot, which SIGHUP reloads in place. `--print-hash`
//! resolves the node's persistent destination locally (no running lnsd
//! needed), prints the hash and the served page paths, and exits. All logic
//! lives in the library's `config`, `content`, `node`, and `web` modules;
//! main only wires them.

use std::process::ExitCode;

use clap::Parser;

use lblogd::cli::Args;
use lblogd::config::Config;
use lblogd::content::{load_snapshot, Reloader};
use lblogd::node::{self, BlogNode};
use lblogd::web;

type MainError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lblogd: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), MainError> {
    let config = Config::load(&args.config)?;
    if args.print_hash {
        return print_hash(&config);
    }
    serve(&config).await
}

/// Print the node's destination hash and the page paths it would serve,
/// then return without starting anything.
///
/// Loading the posts here is what makes this a dry run for publishing: the
/// same parse that serve mode performs, with the same errors, before anything
/// is restarted.
fn print_hash(config: &Config) -> Result<(), MainError> {
    let hash = node::resolve_destination_hash(&config.data_dir)?;
    let snapshot = load_snapshot(&config.posts_dir)?;
    println!("{hash}");
    for path in snapshot.served_paths() {
        println!("{path}");
    }
    Ok(())
}

/// Start the NomadNet node, then run it and the web server concurrently over
/// one shared content snapshot, with SIGHUP reloading it.
async fn serve(config: &Config) -> Result<(), MainError> {
    // A failure here is fatal: at startup there is no previous good state to
    // fall back on. Once running, a failed reload is not (see reload_task).
    let (reloader, content) = Reloader::new(&config.posts_dir)?;

    let blog = BlogNode::start(config.blog_node_config(), content.clone()).await?;
    eprintln!("lblogd: node destination {}", blog.destination_hash());
    for path in blog.served_paths() {
        eprintln!("lblogd: serving {path}");
    }

    // Both sides run forever; a clean return still means the daemon lost a
    // service, so it is treated as an error and try_join aborts the other.
    let node_task = async {
        blog.run()
            .await
            .map_err(|e| MainError::from(format!("node: {e}")))?;
        Err(MainError::from("node: daemon connection closed"))
    };
    let web_task = async {
        web::run_web(config.web_config(), content)
            .await
            .map_err(|e| MainError::from(format!("web: {e}")))?;
        Err(MainError::from("web: server exited unexpectedly"))
    };
    tokio::try_join!(node_task, web_task, reload_task(reloader)).map(|_: ((), (), ())| ())
}

/// Reload the posts on every SIGHUP, for as long as the servers run.
///
/// A failed reload is logged and dropped: the previous snapshot stays live, so
/// a malformed post cannot take a running blog offline. That is the whole
/// point of reloading instead of restarting.
async fn reload_task(reloader: Reloader) -> Result<(), MainError> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sighup = signal(SignalKind::hangup()).map_err(|e| format!("SIGHUP handler: {e}"))?;
    loop {
        // None means the signal stream ended, which cannot happen for SIGHUP
        // while the process lives; treat it as "no more reloads" and idle
        // rather than tearing down the servers.
        if sighup.recv().await.is_none() {
            std::future::pending::<()>().await;
        }
        match reloader.reload() {
            Ok(count) => eprintln!("lblogd: SIGHUP: reloaded {count} posts"),
            Err(e) => eprintln!("lblogd: SIGHUP: reload failed, keeping previous content: {e}"),
        }
    }
}
