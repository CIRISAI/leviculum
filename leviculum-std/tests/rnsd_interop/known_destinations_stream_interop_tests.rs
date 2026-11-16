//! Order 1749 (Refs #232): the streaming flush, against rnsd's own reader
//! and writer.
//!
//! The flush no longer decodes the whole `known_destinations` file into a
//! tree; it reads the old file one entry at a time, writes what this node
//! holds, then copies the entries only the file holds that the reference's
//! cull would keep (`Identity.py:336-352`). Both directions run against the
//! pinned `reference/Reticulum`, offline, the way
//! `known_destinations_age_interop_tests` does: rnsd writes a file through
//! `Identity.save_known_destinations`, we load it with a memory cap below
//! its size and flush, and rnsd reads ours back through
//! `Identity.load_known_destinations`.

use std::collections::BTreeMap;
use std::path::Path;

use leviculum_core::memory_storage::TableCaps;
use leviculum_core::traits::Storage as CoreStorage;
use leviculum_core::Identity;
use leviculum_std::driver::StdStorage;

const VENDOR_RNS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum");

/// `write <age>:<use>...` remembers one fresh identity per argument through
/// `Identity.remember`, sets its announce time `age` seconds back and its
/// fifth element to `-1` (`pin`), `0` (`never`) or a use `<use>` seconds
/// back, and saves. `dump` loads and prints every entry. Both print
/// `ENTRY <hash> <announce> <use> <app_data>`.
const STREAM_PY: &str = r#"
import sys, time
import RNS

RNS.Reticulum.storagepath = sys.argv[1]

def show(dest, e):
    print("ENTRY " + dest.hex() + " " + repr(float(e[0])) + " " + repr(float(e[4])) + " " + (e[3].decode() if e[3] else "-"))

if sys.argv[2] == "write":
    now = time.time()
    for spec in sys.argv[3:]:
        age, use = spec.split(":")
        identity = RNS.Identity()
        dest = RNS.Destination.hash(identity, "kdstream", "rnsd")
        RNS.Identity.remember(RNS.Identity.get_random_hash(), dest, identity.get_public_key(), ("app-" + spec).encode())
        e = RNS.Identity.known_destinations[dest]
        e[0] = now - float(age)
        e[4] = -1 if use == "pin" else (0 if use == "never" else now - float(use))
        show(dest, e)
    RNS.Identity.save_known_destinations()
elif sys.argv[2] == "dump":
    RNS.Identity.load_known_destinations()
    for dest, e in RNS.Identity.known_destinations.items():
        show(dest, e)
else:
    raise SystemExit("unknown op: " + sys.argv[2])
"#;

#[derive(Debug, PartialEq)]
struct Entry {
    announce: f64,
    use_state: f64,
    app_data: String,
}

fn run_python(storage: &Path, args: &[&str]) -> BTreeMap<[u8; 16], Entry> {
    let script = storage.join("known_destinations_stream.py");
    std::fs::write(&script, STREAM_PY).expect("write the python side");
    let output = std::process::Command::new("python3")
        .arg(&script)
        .arg(storage)
        .args(args)
        .env("PYTHONPATH", VENDOR_RNS_ROOT)
        .output()
        .expect("spawn python3");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "python {args:?} must exit 0, got {:?}\nstdout {stdout}\nstderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix("ENTRY "))
        .map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let hash: [u8; 16] = hex::decode(parts[0])
                .expect("hex hash")
                .try_into()
                .expect("16 bytes");
            (
                hash,
                Entry {
                    announce: parts[1].parse().expect("announce float"),
                    use_state: parts[2].parse().expect("use float"),
                    app_data: parts[3].to_string(),
                },
            )
        })
        .collect()
}

/// rnsd writes three recent never-used entries (what our node will hold
/// with a memory cap of three) and three a week old that only the file will
/// hold: one pinned, one used an hour ago, one never used. After our load
/// and one flush with a newcomer, rnsd reads back the held three, the
/// newcomer, and the pinned and the used one with every field to the bit;
/// the stale never-used one is gone, as rnsd's own cull removes it.
#[test]
fn rnsd_reads_our_streamed_flush_of_its_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let week = "604800";
    let written = run_python(
        dir.path(),
        &[
            "write",
            "10:never",
            "20:never",
            "30:never",
            &format!("{week}:pin"),
            &format!("{week}:3600"),
            &format!("{week}:never"),
        ],
    );
    assert_eq!(written.len(), 6);
    let by_spec = |spec: &str| {
        *written
            .iter()
            .find(|(_, e)| e.app_data == format!("app-{spec}"))
            .unwrap_or_else(|| panic!("rnsd wrote no {spec}"))
            .0
    };
    let pinned = by_spec(&format!("{week}:pin"));
    let used = by_spec(&format!("{week}:3600"));
    let stale = by_spec(&format!("{week}:never"));

    let caps = TableCaps {
        identity_cap: 3,
        ..TableCaps::desktop()
    };
    let newcomer = [0x4e; 16];
    {
        let mut storage = StdStorage::new_with_caps(dir.path(), caps).expect("storage");
        CoreStorage::set_identity(
            &mut storage,
            newcomer,
            Identity::generate(&mut rand_core::OsRng),
        );
        CoreStorage::flush(&mut storage);
    }

    let read_back = run_python(dir.path(), &["dump"]);
    for (hash, entry) in &written {
        if *hash == stale {
            assert!(
                !read_back.contains_key(hash),
                "the stale never-used entry is culled"
            );
            continue;
        }
        assert_eq!(
            read_back.get(hash),
            Some(entry),
            "{}: rnsd reads back what it wrote",
            hex::encode(hash)
        );
    }
    assert!(read_back.contains_key(&newcomer), "the newcomer is written");
    assert!(read_back.contains_key(&pinned) && read_back.contains_key(&used));
    assert_eq!(read_back.len(), 6, "held 3 + newcomer + pinned + used");
}
