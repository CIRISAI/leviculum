//! Can `lnpnd` serve messages out of a store Python `lxmd` left behind?
//!
//! The question is not academic: `leviculum.network` is to swap `lxmd` for
//! `lnpnd`, and every message in the old store is somebody's undelivered
//! mail. Either our reader picks them up or the swap starts empty, and which
//! of those it is has to be a measurement rather than a reading of the code.
//!
//! The store under test is written by `scripts/make-lxmd-messagestore.py`,
//! which drives the reference's own ingest chain
//! (`LXStamper.validate_pn_stamps` then `LXMRouter.lxmf_propagation`,
//! `reference/LXMF/LXMF/LXMRouter.py:2512-2515`) — the only place the
//! reference ever creates a store file. A fixture written here would prove
//! nothing except that we can read our own idea of the format.
//!
//! Two properties of a real store this pins:
//!
//! 1. The receive timestamp in the filename is a Python **float**
//!    (`time.time()`), not an integer.
//! 2. At stamp value 0 the reference omits the value component entirely, so
//!    the name has two components — and its own re-index then skips such
//!    files (`enable_propagation`, `:565-592`, requires three). Matching
//!    that is correct: those messages are lost to `lxmd` too.

use std::path::{Path, PathBuf};
use std::process::Command;

use leviculum_lxmf::propagation_store::PropagationStore;
use leviculum_std::FilePropagationStore;

/// One message the generator reported writing.
struct Written {
    transient_id: [u8; 32],
    stamp_value: u8,
}

/// Run the generator into `dir`, or `None` when the reference submodules are
/// not checked out (a plain clone without `--recursive`).
fn generate(dir: &Path) -> Option<(PathBuf, Vec<Written>)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    if !root.join("reference/LXMF/LXMF").is_dir() || !root.join("reference/Reticulum/RNS").is_dir()
    {
        eprintln!("reference/LXMF or reference/Reticulum not checked out, skipping");
        return None;
    }
    let output = Command::new("python3")
        .arg(root.join("scripts/make-lxmd-messagestore.py"))
        .arg(dir)
        .output()
        .expect("python3 runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the reference refused to build its own store:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let store_dir = PathBuf::from(lines.pop().expect("the generator prints the store path"));
    let written = lines
        .iter()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let hex = fields.next().expect("transient id");
            let value: u8 = fields
                .next()
                .expect("stamp value")
                .parse()
                .expect("a number");
            let mut transient_id = [0u8; 32];
            for (index, byte) in transient_id.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&hex[2 * index..2 * index + 2], 16).expect("hex");
            }
            Written {
                transient_id,
                stamp_value: value,
            }
        })
        .collect();
    Some((store_dir, written))
}

#[test]
fn lnpnd_reads_a_store_python_lxmd_wrote() {
    let work = tempfile::tempdir().expect("temp dir");
    let Some((store_dir, written)) = generate(work.path()) else {
        return;
    };

    // The three-component names: the messages the reference would itself
    // recover on restart. Anything fewer and the takeover is a no-op.
    let expected: Vec<&Written> = written.iter().filter(|w| w.stamp_value > 0).collect();
    assert!(
        !expected.is_empty(),
        "the generator must produce at least one message with a non-zero stamp value"
    );

    let store = FilePropagationStore::open(&store_dir, 10_000_000).expect("the store opens");
    assert_eq!(
        store.len(),
        expected.len(),
        "every message lxmd could recover must be recovered here too; \
         the store held {} file(s)",
        std::fs::read_dir(&store_dir)
            .map(|d| d.count())
            .unwrap_or(0)
    );

    let mut recovered = Vec::new();
    store
        .for_each(&mut |meta| recovered.push(*meta))
        .expect("iterates");
    for message in &expected {
        let meta = recovered
            .iter()
            .find(|meta| meta.transient_id == message.transient_id)
            .unwrap_or_else(|| panic!("message {:02x?} is missing", &message.transient_id[..4]));
        assert_eq!(meta.stamp_value, message.stamp_value);
        assert!(
            meta.received_at > 1_700_000_000,
            "the float receive timestamp must survive as whole seconds, got {}",
            meta.received_at
        );
        // The body is what a peer or client gets served, byte for byte.
        let body = store
            .read_body(&message.transient_id)
            .expect("the body reads")
            .expect("the body is there");
        assert_eq!(body.len(), meta.size as usize);
        assert_eq!(
            &body[..16],
            &meta.destination_hash[..],
            "the first 16 bytes are the destination hash, as the reference reads them back"
        );
    }
}

/// Reopening must not renumber: a foreign three-component name is assigned a
/// sequence on first open and that assignment is persisted, or every restart
/// re-offers the whole store to every peer.
#[test]
fn a_reopened_foreign_store_keeps_its_sequences() {
    let work = tempfile::tempdir().expect("temp dir");
    let Some((store_dir, _)) = generate(work.path()) else {
        return;
    };

    let first = FilePropagationStore::open(&store_dir, 10_000_000).expect("opens");
    let mut before: Vec<([u8; 32], u64)> = Vec::new();
    first
        .for_each(&mut |meta| before.push((meta.transient_id, meta.sequence)))
        .expect("iterates");
    drop(first);

    let second = FilePropagationStore::open(&store_dir, 10_000_000).expect("reopens");
    let mut after: Vec<([u8; 32], u64)> = Vec::new();
    second
        .for_each(&mut |meta| after.push((meta.transient_id, meta.sequence)))
        .expect("iterates");

    before.sort();
    after.sort();
    assert_eq!(
        before, after,
        "a reopen must not renumber a recovered store"
    );
    assert!(
        before.iter().all(|(_, sequence)| *sequence > 0),
        "every recovered message needs a sequence a peer cursor can index"
    );
}
