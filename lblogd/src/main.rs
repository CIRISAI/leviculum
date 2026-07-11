//! `lblogd` binary: serve the blog over Reticulum and the clearnet.
//!
//! Serve mode starts the NomadNet page node and the HTTP/HTTPS web server
//! concurrently and runs until either fails; both are daemons, so any return
//! is an error and the process exits non zero. `--print-hash` resolves the
//! node's persistent destination locally (no running lnsd needed), prints
//! the hash and the served page paths, and exits. All logic lives in the
//! library's `config`, `node`, and `web` modules; main only wires them.

use std::process::ExitCode;

use clap::Parser;

use lblogd::cli::Args;
use lblogd::config::Config;
use lblogd::node::{self, BlogNode};
use lblogd::post::load_posts_dir;
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
fn print_hash(config: &Config) -> Result<(), MainError> {
    let hash = node::resolve_destination_hash(&config.data_dir)?;
    let posts = load_posts_dir(&config.posts_dir)?;
    println!("{hash}");
    for path in node::page_paths(&posts) {
        println!("{path}");
    }
    Ok(())
}

/// Start the NomadNet node, then run it and the web server concurrently.
async fn serve(config: &Config) -> Result<(), MainError> {
    let blog = BlogNode::start(config.blog_node_config()).await?;
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
        web::run_web(config.web_config())
            .await
            .map_err(|e| MainError::from(format!("web: {e}")))?;
        Err(MainError::from("web: server exited unexpectedly"))
    };
    tokio::try_join!(node_task, web_task).map(|_: ((), ())| ())
}
