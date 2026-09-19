//! Doc guard: the board facts in `lnflash/catalogue.toml` and the board
//! facts in the book are the same facts.
//!
//! Codeberg #262. The hardware-coverage work put the same handful of
//! strings in three places at once — `lnflash/catalogue.toml`, the coverage
//! page `docs/src/firmware/boards.md`, and the flashing pages under
//! `docs/src/concepts/` and `docs/src/firmware/` — and nothing held them
//! together. The issue names the precedent in this very tree: eleven
//! `Justfile` citations in `flashing.md` had drifted by roughly 250 lines
//! before anyone noticed, because the citation guard only length-checks a
//! citation that names nothing.
//!
//! The drift that matters here is not a line number. It is a `Board-ID` or
//! a USB ID that changed in the catalogue — the file a write decision
//! actually rests on — while the book kept saying the old one. Both halves
//! look healthy on their own; the reader is simply told something that is
//! no longer true about the string their board publishes.
//!
//! # The two claims
//!
//! 1. **Every board the catalogue knows is on the coverage page.** The
//!    catalogue is what `lnflash` will talk to or write; `boards.md` is
//!    where a user goes to find out which hardware we serve. A board that
//!    exists in the first and not the second is exactly the hole #262
//!    opened with: "we build firmware for two boards and never wrote down
//!    what else it runs on."
//!
//! 2. **Every identifier a session rests on is written down in the book.**
//!    The `Board-ID` a write is allowed to match, the USB IDs a bootloader
//!    and a running application answer on, the mass-storage label a user is
//!    told to look for. Change one in the catalogue and the sentence in the
//!    book that quotes it goes stale in the same commit.
//!
//! Plus the pointers that run the other way: a catalogue entry may send a
//! person to a section of the book (`flashing.double_tap.docs`), and that
//! file and that heading have to still be there.
//!
//! # What this guard does not claim
//!
//! It checks that each catalogue value *occurs* in the book, not that the
//! sentence around it is right. A book that keeps a superseded `Board-ID`
//! in a paragraph beside the current one still passes. That direction is
//! not mechanical here and should not be faked: the book deliberately
//! quotes identifiers that are not ours and must not be in the catalogue —
//! Meshtastic's `2886:0059`, the XIAO bootloader's `2886:0044`, LILYGO's
//! `TTGO_eink` — so "every board-ID-shaped token in the book is a
//! catalogue key" would be false by design. The direction that is checked
//! is the one where the machine-readable side leads and the prose follows,
//! which is the direction a board change actually travels.
//!
//! The coverage assertions at the end are the positive control for the
//! corpus: without them this passes vacuously the day the book moves and
//! `docs/src` reads as empty.

use std::fs;
use std::path::{Path, PathBuf};

use lnflash::manifest::Catalogue;

/// Where a user looks up which hardware we serve.
const COVERAGE_PAGE: &str = "docs/src/firmware/boards.md";

/// The book's sources. `docs/book` is generated output and is not read.
const BOOK_SRC: &str = "docs/src";

/// One string the catalogue states and the book has to state too.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fact {
    board: String,
    field: &'static str,
    value: String,
}

impl Fact {
    fn new(board: &str, field: &'static str, value: &str) -> Self {
        Self {
            board: board.to_string(),
            field,
            value: value.to_string(),
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .to_path_buf()
}

fn markdown_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            markdown_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// Every markdown file of the book, read once.
fn book() -> Vec<(PathBuf, String)> {
    let root = repo_root();
    let mut paths = Vec::new();
    markdown_under(&root.join(BOOK_SRC), &mut paths);
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| fs::read_to_string(&p).ok().map(|text| (p, text)))
        .collect()
}

fn catalogue() -> Catalogue {
    Catalogue::builtin().expect("the compiled-in catalogue parses")
}

/// Every identifier a session rests on, board by board.
///
/// Both halves of an entry are here, and for the same reason the catalogue
/// splits them: `candidate_usb` is what a control session addresses a
/// running board by, the flashing half is what a write matches on. A user
/// reading about either is reading a string out of this file.
fn identifiers(catalogue: &Catalogue) -> Vec<Fact> {
    let mut facts = Vec::new();
    for name in catalogue.names() {
        let board = catalogue
            .board(name)
            .expect("a name the catalogue just listed");
        for id in &board.candidate_usb {
            facts.push(Fact::new(name, "candidate_usb", id));
        }
        let Some(flashing) = board.flashing() else {
            continue;
        };
        facts.push(Fact::new(
            name,
            "flashing.identify.info_uf2_board_id",
            &flashing.identify.info_uf2_board_id,
        ));
        for id in &flashing.identify.bootloader_usb {
            facts.push(Fact::new(name, "flashing.identify.bootloader_usb", id));
        }
        if let Some(label) = &flashing.identify.msc_label {
            facts.push(Fact::new(name, "flashing.identify.msc_label", label));
        }
    }
    facts
}

/// The facts no file of the corpus states. Separated from the corpus it
/// runs over so the guard itself can be made red on demand — see
/// `the_guard_reports_an_identifier_the_book_does_not_state`.
fn undocumented<'f>(facts: &'f [Fact], corpus: &[(PathBuf, String)]) -> Vec<&'f Fact> {
    facts
        .iter()
        .filter(|fact| !corpus.iter().any(|(_, text)| text.contains(&fact.value)))
        .collect()
}

#[test]
fn every_board_the_catalogue_knows_is_named_on_the_coverage_page() {
    let page = fs::read_to_string(repo_root().join(COVERAGE_PAGE))
        .expect("the hardware-coverage page is in the tree");
    let catalogue = catalogue();
    let names = catalogue.names();
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !page.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "{COVERAGE_PAGE} names none of {missing:?}, which the catalogue does. \
         A board lnflash will talk to that the coverage page does not mention \
         is the hole Codeberg #262 opened with."
    );
    assert!(
        names.len() >= 3,
        "the catalogue lists {} boards; this guard was written against three \
         and passing with fewer means it stopped checking anything",
        names.len()
    );
}

#[test]
fn every_identifier_a_session_rests_on_is_written_down_in_the_book() {
    let corpus = book();
    let catalogue = catalogue();
    let facts = identifiers(&catalogue);
    let missing = undocumented(&facts, &corpus);
    assert!(
        missing.is_empty(),
        "these catalogue identifiers appear nowhere under {BOOK_SRC}:\n{}\n\
         Either the book still quotes the value they replaced, or a board was \
         added to lnflash without being written down (Codeberg #262).",
        missing
            .iter()
            .map(|f| format!("  {} {} = {:?}", f.board, f.field, f.value))
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!(
        "doc_board_catalogue: {} identifiers across {} boards, against {} files",
        facts.len(),
        catalogue.names().len(),
        corpus.len()
    );
    assert!(
        facts.len() >= 8,
        "only {} identifiers were checked; the catalogue states more than \
         that and a shrinking count means the reader stopped reading it",
        facts.len()
    );
    assert!(
        corpus.len() >= 20,
        "only {} markdown files were read under {BOOK_SRC}; the book is \
         larger than that, so this ran against the wrong tree",
        corpus.len()
    );
}

#[test]
fn every_section_the_catalogue_sends_a_person_to_still_exists() {
    let root = repo_root();
    let catalogue = catalogue();
    let mut checked = 0usize;
    for name in catalogue.names() {
        let board = catalogue
            .board(name)
            .expect("a name the catalogue just listed");
        let Some(pointer) = board.flashing().and_then(|f| f.double_tap.docs.as_deref()) else {
            continue;
        };
        // `<path>, "<heading>"` — the path always, the heading where the
        // entry names one. A pointer at a file is worth little when the
        // section it means has been retitled out from under it.
        let (path, heading) = match pointer.split_once(", \"") {
            Some((path, rest)) => (path.trim(), rest.trim_end_matches('"')),
            None => (pointer.trim(), ""),
        };
        let text = fs::read_to_string(root.join(path)).unwrap_or_else(|e| {
            panic!("board {name}: flashing.double_tap.docs points at {path}, which cannot be read: {e}")
        });
        if !heading.is_empty() {
            assert!(
                text.contains(heading),
                "board {name}: flashing.double_tap.docs points at {path:?} section \
                 {heading:?}, which that file no longer contains"
            );
        }
        checked += 1;
    }
    assert_eq!(
        checked, 1,
        "expected exactly one catalogue pointer into the book (the Pocket V2's \
         hidden pinhole); a changed count means this guard is checking a \
         different set than it was written for"
    );
}

#[test]
fn the_guard_reports_an_identifier_the_book_does_not_state() {
    // The positive control. Without it the two checks above prove only that
    // `contains` returns true somewhere, and a guard nobody has seen fail is
    // a guard nobody knows is wired up.
    let facts = vec![
        Fact::new("t114", "flashing.identify.info_uf2_board_id", "HT-n5262"),
        Fact::new("t114", "candidate_usb", "1209:0001"),
    ];
    let corpus = vec![(
        PathBuf::from("docs/src/firmware/boards.md"),
        "the bootloader publishes HT-n5262 and nothing else here".to_string(),
    )];
    let missing = undocumented(&facts, &corpus);
    assert_eq!(missing.len(), 1, "one of the two is absent from the corpus");
    assert_eq!(missing[0].value, "1209:0001");
    assert!(undocumented(&facts[..1], &corpus).is_empty());
}
