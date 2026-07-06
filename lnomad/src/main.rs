//! `lnomad` binary: a thin Phase 3 skeleton.
//!
//! It renders a local `.mu` file passed as the first argument and prints the
//! ANSI result plus a numbered link index. Fetching a page over Reticulum and
//! the interactive browser loop arrive in Phase 4; this entry point exists so
//! the crate builds as a `bin` and the renderer can be exercised by hand.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(path) => path,
        None => {
            eprintln!("usage: lnomad <page.mu>");
            return ExitCode::from(2);
        }
    };

    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("lnomad: cannot read {path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    let doc = leviculum_micron::parse(&source);
    let page = lnomad::render::render(&doc, 80);
    print!("{}", page.text);

    if !page.links.is_empty() {
        println!("\nLinks:");
        for link in &page.links {
            println!("  [{}] {} -> {}", link.index, link.label, link.target);
        }
    }

    ExitCode::SUCCESS
}
