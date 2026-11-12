//! lnmapcut — writes the map store `lnmapd` serves: cuts a region file
//! into content-addressed pieces and signs the index
//! (`docs/src/concepts/map-server.md`, "The cutter").
//!
//! The region itself is cut from an archive by `leviculum-map-tool cut`
//! (one cut path for the whole tree); this program takes the finished
//! `.lmr` and owns the store side: the split, the hashes, the signed
//! index. Cutting and serving stay separate programs — the store at
//! home runs this by hand or from cron, `lnmapd` only ever reads.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use lnmapd::node::load_or_generate_identity;
use lnmapd::store::{add_pack, AddOptions, Index, KIND_VECTORS};

#[derive(Parser, Debug)]
#[command(
    name = "lnmapcut",
    version,
    about = "Cuts a region file into a signed map store for lnmapd",
    long_about = "Splits a region file (.lmr, version 4 or newer: cut by \
                  leviculum-map-tool) at zoom boundaries into immutable pieces \
                  named by their SHA-256, writes them into the store, and signs \
                  a fresh index with the server identity — the same identity \
                  that owns the lev.maps destination a device pins."
)]
struct Args {
    /// The store directory to write (created as needed).
    #[arg(long)]
    store: PathBuf,

    /// The server identity file (generated on first use). lnmapd
    /// announces lev.maps with the same file.
    #[arg(long)]
    identity: PathBuf,

    /// The pack id in the catalogue, e.g. "berlin".
    #[arg(long)]
    id: String,

    /// The server's HTTP base URL to name in the signed index (one
    /// value configures both paths; a device's map_source overrides
    /// it). Kept from the previous index when not given.
    #[arg(long)]
    http_url: Option<String>,

    /// The region file to add (.lmr, version 4 or newer).
    region: PathBuf,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(index) => {
            let Some(pack) = index.pack(&args.id) else {
                // add_pack puts the pack in the index it returns; not
                // finding it would be a bug in this crate.
                eprintln!(
                    "lnmapcut: pack {} missing from the index it was added to",
                    args.id
                );
                return ExitCode::FAILURE;
            };
            println!(
                "{}: {} pieces, {} bytes, zooms {}..={}, generation {}",
                pack.id,
                pack.pieces.len(),
                pack.total,
                pack.min_zoom,
                pack.max_zoom,
                index.generation
            );
            for (n, piece) in pack.pieces.iter().enumerate() {
                println!(
                    "  piece {n}: completes z{}, {} bytes, {}",
                    piece.zoom,
                    piece.len,
                    piece.id()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lnmapcut: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<Index, String> {
    let identity = load_or_generate_identity(&args.identity).map_err(|e| e.to_string())?;
    let region =
        std::fs::read(&args.region).map_err(|e| format!("read {}: {e}", args.region.display()))?;
    add_pack(
        &args.store,
        &region,
        &identity,
        &AddOptions {
            id: args.id.clone(),
            kind: KIND_VECTORS.to_owned(),
            http_url: args.http_url.clone(),
        },
    )
    .map_err(|e| e.to_string())
}
