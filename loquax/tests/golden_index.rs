//! The goldens' index (1451): the section "The goldens' index" of
//! `docs/src/concepts/loquax-ui.md` lists every scene with its golden per
//! target, so a walk that cannot run a target reads the list instead of
//! guessing file names. This test holds the list to the files: a name in
//! it with no file, a golden file it does not name, and a terminal golden
//! without its German twin are each refused by name.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const BEGIN: &str = "<!-- goldens-index:begin -->";
const END: &str = "<!-- goldens-index:end -->";

/// The workspace's root, one above this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the workspace")
        .to_path_buf()
}

/// Whether `name` is a golden's file name: a picture, a screen, a scene,
/// a session's frames or a style map.
fn is_golden(name: &str) -> bool {
    [".png", ".txt", ".json", ".frames", ".frame", ".styles"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

/// The directories goldens live in, and whether every file there is one
/// (the fixtures directory also holds a session's inputs: photos, a
/// voice message, the script that makes a photo).
fn dirs() -> [(PathBuf, bool); 4] {
    let root = root();
    [
        (root.join("loquax/tests/golden"), true),
        (root.join("loquax/tests/fixtures"), false),
        (root.join("loquax-m9/tests/golden"), true),
        (root.join("loquax-android/tests/golden"), true),
    ]
}

/// Every golden on disk, by file name.
fn on_disk() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (dir, all) in dirs() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let name = entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned();
            let golden = match all {
                true => name != "README.md",
                false => {
                    name.ends_with(".frames")
                        || name.ends_with(".frame")
                        || name.ends_with(".styles")
                }
            };
            if golden {
                assert!(
                    names.insert(name.clone()),
                    "{name} is in two golden directories: the index names a file, not a path"
                );
            }
        }
    }
    names
}

/// The index's section of the page.
fn section(page: &str) -> &str {
    let start = page.find(BEGIN).expect("the index's begin marker") + BEGIN.len();
    let end = page.find(END).expect("the index's end marker");
    assert!(start < end, "the markers stand in order");
    &page[start..end]
}

/// Every golden file name the section names: what stands in backticks.
fn named(section: &str) -> BTreeSet<String> {
    section
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|word| !word.contains(' ') && is_golden(word))
        .map(str::to_string)
        .collect()
}

/// The page as it is on disk.
fn page() -> String {
    let path = root().join("docs/src/concepts/loquax-ui.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_index_names_every_golden_and_only_goldens_that_exist() {
    let page = page();
    let named = named(section(&page));
    let disk = on_disk();
    let missing: Vec<_> = named.difference(&disk).collect();
    let unnamed: Vec<_> = disk.difference(&named).collect();
    assert!(
        missing.is_empty(),
        "the goldens' index in loquax-ui.md names files that are not there: {missing:?}"
    );
    assert!(
        unnamed.is_empty(),
        "goldens the index in loquax-ui.md does not name; give each its scene's row: {unnamed:?}"
    );
}

/// The German twin a terminal golden must have, or `None` when `name` is
/// no terminal golden or is a twin itself. The style map pins colours,
/// not words, and has none.
fn twin(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    let terminal = (name.starts_with("tui-") || name.starts_with("tracks-")) && ext == "txt"
        || name.starts_with("tui_") && (ext == "frames" || ext == "frame");
    let twin = stem.ends_with("-de") || stem.ends_with("_de");
    match (terminal, twin, name.starts_with("tui_")) {
        (true, false, true) => Some(format!("{stem}_de.{ext}")),
        (true, false, false) => Some(format!("{stem}-de.{ext}")),
        _ => None,
    }
}

#[test]
fn every_terminal_golden_has_its_german_twin() {
    let disk = on_disk();
    let alone: Vec<_> = disk
        .iter()
        .filter_map(|name| twin(name).filter(|t| !disk.contains(t)).map(|t| (name, t)))
        .collect();
    assert!(
        alone.is_empty(),
        "terminal goldens without the German frame a desktop under LANG=de_DE.UTF-8 draws: {alone:?}"
    );
}

/// The checks above against a page and a disk that differ from the real
/// ones: a name with no file and a file with no name are found, a twin is
/// asked of an English terminal golden and of nothing else.
#[test]
fn the_index_check_finds_what_it_is_for() {
    let page = page();
    let real = named(section(&page));
    assert!(real.contains("gui-reply-sent.png") && real.contains("tui_chat_de.frames"));
    assert!(!real.contains("WelcomeActivity.java"), "only goldens count");
    let with_ghost = page.replace(BEGIN, &format!("{BEGIN}\n`gui-no-such-scene.png`"));
    assert!(named(section(&with_ghost)).contains("gui-no-such-scene.png"));
    let without = page.replace("`gui-reply-sent.png`", "");
    assert!(on_disk()
        .difference(&named(section(&without)))
        .any(|n| n == "gui-reply-sent.png"));
    assert_eq!(
        twin("tui-settings.txt").as_deref(),
        Some("tui-settings-de.txt")
    );
    assert_eq!(
        twin("tracks-park.txt").as_deref(),
        Some("tracks-park-de.txt")
    );
    assert_eq!(twin("tui_map.frames").as_deref(), Some("tui_map_de.frames"));
    assert_eq!(
        twin("tui_toast_two_legs.frame").as_deref(),
        Some("tui_toast_two_legs_de.frame")
    );
    assert_eq!(twin("tui-settings-de.txt"), None);
    assert_eq!(twin("tui_chat.styles"), None);
    assert_eq!(twin("gui-chat-light.png"), None);
    assert_eq!(twin("scene-reply.json"), None);
}
