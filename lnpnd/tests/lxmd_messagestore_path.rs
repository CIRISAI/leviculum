//! lnpnd started on an `lxmd` config directory must open the store `lxmd`
//! left there.
//!
//! `lxmd` keeps it at `<configdir>/storage/lxmf/messagestore`
//! (`reference/LXMF/LXMF/Utilities/lxmd.py:338`,
//! `reference/LXMF/LXMF/LXMRouter.py:130/555`); lnpnd's data directory
//! defaults to `<configdir>/storage`, so `lnpnd::message_store_dir` has to
//! land on the same path, and a store lnpnd itself kept at the old
//! `<data-dir>/messagestore` has to follow it there.

use std::path::PathBuf;
use std::process::Command;

use leviculum_lxmf::propagation_store::PropagationStore;
use leviculum_std::FilePropagationStore;

#[test]
fn a_fresh_start_creates_the_lxmd_path() {
    let data = tempfile::tempdir().expect("temp data dir");
    let store_dir = lnpnd::message_store_dir(data.path()).expect("resolves");
    assert_eq!(
        store_dir.path,
        data.path().join("lxmf").join("messagestore")
    );
    assert_eq!(store_dir.moved_from, None);

    let store = FilePropagationStore::open(&store_dir.path, 100_000).expect("opens");
    assert!(store.is_empty());
    assert!(data.path().join("lxmf/messagestore").is_dir());
    assert!(!data.path().join("messagestore").exists());
}

#[test]
fn an_old_layout_store_is_moved_and_keeps_its_messages() {
    let data = tempfile::tempdir().expect("temp data dir");
    let old = data.path().join("messagestore");
    let mut store = FilePropagationStore::open(&old, 100_000).expect("old store opens");
    for index in 1..=3u8 {
        let mut body = vec![index; 120];
        body[16..].fill(0xCD);
        store
            .append(&[index; 32], 1_789_000_000 + index as u64, 0, &body)
            .expect("append");
    }
    drop(store);

    let store_dir = lnpnd::message_store_dir(data.path()).expect("resolves");
    assert_eq!(store_dir.moved_from.as_deref(), Some(old.as_path()));
    assert_eq!(
        store_dir.path,
        data.path().join("lxmf").join("messagestore")
    );
    assert!(!old.exists(), "the old directory is moved, not copied");

    let store = FilePropagationStore::open(&store_dir.path, 100_000).expect("opens");
    assert_eq!(store.len(), 3);
    assert!(store.contains(&[2; 32]).expect("contains"));

    // Once: a second start finds the store already in place.
    drop(store);
    let again = lnpnd::message_store_dir(data.path()).expect("resolves again");
    assert_eq!(again.moved_from, None);
    assert_eq!(again.path, store_dir.path);
}

/// When both exist, the new path wins and the old directory is left for the
/// operator rather than merged or overwritten.
#[test]
fn an_old_store_beside_a_new_one_is_left_alone() {
    let data = tempfile::tempdir().expect("temp data dir");
    std::fs::create_dir_all(data.path().join("messagestore")).expect("old");
    std::fs::create_dir_all(data.path().join("lxmf/messagestore")).expect("new");
    let store_dir = lnpnd::message_store_dir(data.path()).expect("resolves");
    assert_eq!(store_dir.moved_from, None);
    assert!(data.path().join("messagestore").is_dir());
}

/// The positive control against the reference itself: the store lxmd's own
/// router writes under `<configdir>/storage` is the directory lnpnd opens
/// with its default data directory, and every message in it is there.
#[test]
fn the_default_data_dir_opens_the_store_lxmd_wrote() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    if !root.join("reference/LXMF/LXMF").is_dir() || !root.join("reference/Reticulum/RNS").is_dir()
    {
        eprintln!("reference/LXMF or reference/Reticulum not checked out, skipping");
        return;
    }
    let configdir = tempfile::tempdir().expect("temp config dir");
    let output = Command::new("python3")
        .arg(root.join("scripts/make-lxmd-messagestore.py"))
        .arg(configdir.path())
        .output()
        .expect("python3 runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the reference refused to build its store:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let written_path = PathBuf::from(lines.pop().expect("the generator prints the store path"));
    let written = lines.len();

    // lnpnd's default: `--data-dir` unset means `<configdir>/storage`.
    let store_dir = lnpnd::message_store_dir(&configdir.path().join("storage")).expect("resolves");
    assert_eq!(
        std::fs::canonicalize(&store_dir.path).expect("lnpnd's path exists"),
        std::fs::canonicalize(&written_path).expect("lxmd's path exists"),
        "lnpnd must open the directory lxmd wrote"
    );
    let store = FilePropagationStore::open(&store_dir.path, 10_000_000).expect("opens");
    assert_eq!(
        store.len(),
        written,
        "every message lxmd wrote is in the store"
    );
}
