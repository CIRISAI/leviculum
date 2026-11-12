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
use lnmapd::store::{add_pack, AddOptions, Index, KIND_ELEVATION, KIND_VECTORS};

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

    /// Let the file replace a pack of another kind under the same id
    /// (a region by heights, or heights by a region). Without it such an
    /// add is refused and the store is left as it was.
    #[arg(long)]
    replace: bool,

    /// The file to add: a region file (.lmr, version 4 or newer) or a
    /// height file (`leviculum-map-tool heights`), told apart by their
    /// magic.
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
                // A height pack's directory completes no zoom; every
                // block after it is complete at all its zooms.
                let what = if pack.kind == KIND_ELEVATION && n == 0 {
                    "directory".to_owned()
                } else {
                    format!("completes z{}", piece.zoom)
                };
                println!("  piece {n}: {what}, {} bytes, {}", piece.len, piece.id());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lnmapcut: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The pack kind a file's magic names: a height file's directory starts
/// with `LMHD`, everything else is taken as a region file (whose own
/// reader then refuses what it is not).
fn kind_of(file: &[u8]) -> &'static str {
    if file.starts_with(&leviculum_map::elevation::DIRECTORY_MAGIC) {
        KIND_ELEVATION
    } else {
        KIND_VECTORS
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
            kind: kind_of(&region).to_owned(),
            http_url: args.http_url.clone(),
            replace: args.replace,
        },
    )
    .map_err(|e| e.to_string())
}
