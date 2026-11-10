//! `--seed-home <dir>` (1562): the bench home's archive. The bench M9 is
//! never paired with a real home again (window 13 of 2026-11-08 sent read
//! marks for Lew's messages from the bench); it joins a `loquax mailbox`
//! of its own on the bench host, whose state directory this fills: an
//! identity of its own (made when there is none), and the 1489 fixture
//! (300 conversations, 4000 messages, the names and the read marks)
//! written into its archive under that identity's LXMF address, as the
//! home would have taken it in. `scripts/m9-bench-home.sh setup` calls
//! it, then the mailbox runs on the directory and the board pulls the
//! fixture as it pulls any home's archive.

use std::path::Path;

use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire;
use loquax::home::{KIND_MARK, KIND_NAME};
use loquax::store::Store;

use crate::real;

/// What a seed did: the home's address and the archive's entries.
pub struct Seeded {
    pub address: [u8; 16],
    pub appended: u64,
    pub held: u64,
}

/// Seed the mailbox state directory `dir`. An archive that holds entries
/// already is left as it is (a second setup changes nothing).
pub fn seed(dir: &Path) -> Result<Seeded, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let identity = loquax::identity::load_or_create(&dir.join("identity"))
        .map_err(|e| format!("{}: {e:?}", dir.join("identity").display()))?;
    let address = loquax::home::lxmf_address(&identity.public_key_bytes())?;
    let mut store = Store::open_in(dir, address).map_err(|e| format!("{e:?}"))?;
    let held = store
        .archive_since(0, usize::MAX)
        .map_err(|e| format!("{e:?}"))?
        .entries
        .len() as u64;
    if held > 0 {
        return Ok(Seeded {
            address,
            appended: 0,
            held,
        });
    }
    let mut appended = 0;
    for entry in real::archive_for(address)? {
        let kept = match entry.kind {
            KIND_NAME => {
                let (peer, name) = wire::read_name_entry(&entry.packed)
                    .map_err(|e| format!("a fixture name: {e:?}"))?;
                store.archive_name(peer, &name, entry.at, u64::MAX)
            }
            KIND_MARK => {
                // As the home keeps a device's mark: under the hash of
                // its bytes (`home::server`, `keep_mark`).
                let id = leviculum_core::crypto::sha256(&entry.packed);
                store.archive_append(KIND_MARK, &id, &entry.packed, entry.at, u64::MAX)
            }
            kind => {
                let id = Message::unpack(&entry.packed, None, None, DeliveryMethod::Direct)
                    .map_err(|e| format!("a fixture message: {e:?}"))?
                    .message_id;
                store.archive_append(kind, &id, &entry.packed, entry.at, u64::MAX)
            }
        }
        .map_err(|e| format!("{e:?}"))?;
        appended += u64::from(kept.is_some());
    }
    Ok(Seeded {
        address,
        appended,
        held: appended,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seed writes the fixture under the directory's own identity:
    /// every message is to or from that address, the archive reads back
    /// whole, and a second seed adds nothing.
    #[test]
    fn the_bench_home_holds_the_fixture_under_its_own_address() {
        let dir = tempfile::tempdir().expect("dir");
        let first = seed(dir.path()).expect("seed");
        assert!(first.appended > 4000, "{}", first.appended);
        let again = seed(dir.path()).expect("again");
        assert_eq!(
            (again.address, again.appended, again.held),
            (first.address, 0, first.appended)
        );
        let store = Store::open_in(dir.path(), first.address).expect("store");
        let page = store.archive_since(0, usize::MAX).expect("archive");
        let mut ours = 0;
        for entry in &page.entries {
            if let Some(event) = loquax::home::board::fold(entry, &first.address) {
                let _ = event;
                ours += 1;
            }
        }
        assert!(
            ours > 4000,
            "the board folds the fixture as this home's: {ours}"
        );
        // The control: under another address the received ones fold not.
        let other = [7u8; 16];
        let theirs = page
            .entries
            .iter()
            .filter(|e| e.kind == loquax::home::KIND_RECEIVED)
            .filter(|e| loquax::home::board::fold(e, &other).is_some())
            .count();
        assert_eq!(theirs, 0);
    }
}
