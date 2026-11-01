//! The device walk's new phone (1281, walk finding 24): takes the mailbox
//! over from the phone in the walk, with the code its "Move to a new phone"
//! shows, as the new phone's first start does
//! ([`loquax_android::mailbox::move_in`]).
//!
//! ```sh
//! cargo run -p loquax-android --example walk_move -- DIR CODE NAME
//! ```
//!
//! `DIR` stands in for the new phone's files directory; `NAME` is how the
//! old phone names it. Exit 0 once the old phone confirmed the move.

use std::path::PathBuf;
use std::process::ExitCode;

fn run(dir: PathBuf, code: &str, name: &str) -> Result<(), String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("no runtime: {e}"))?;
    loquax_android::mailbox::move_in(&dir, code, name, runtime.handle())
        .map_err(|failed| format!("{failed:?}"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, code, name] = args.as_slice() else {
        eprintln!("usage: walk_move DIR CODE NAME");
        return ExitCode::from(2);
    };
    match run(PathBuf::from(dir), code, name) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("walk_move: {error}");
            ExitCode::FAILURE
        }
    }
}
