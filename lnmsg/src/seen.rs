//! The cross-run seen-message store, and why it exists.
//!
//! Within one run the router de-duplicates for itself. Across runs it
//! cannot: `lnmsg` builds a fresh router per invocation, so a message that
//! was delivered directly yesterday and is still sitting in a mailbox today
//! (the sender's fallback uploaded a copy, the peer came back before the
//! drain) would print twice. This store is the memory that survives the
//! process: one lowercase-hex message id per line, under
//! `${LNMSG_HOME}/seen`.
//!
//! The message id is the right key — not the transient id — because it is
//! the one identity a message keeps across its delivery methods: the direct
//! copy and the mailbox copy of one message share it by construction
//! (`leviculum-lxmf/src/message.rs`, the id covers the signed content).

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The store's file name under the lnmsg home directory.
pub const SEEN_FILE: &str = "seen";

/// Compaction threshold: at this many entries, loading rewrites the file to
/// the newest [`KEEP_ON_COMPACT`]. A messenger that has printed 65k messages
/// has long rotated its mailbox contents; unbounded growth is the only thing
/// this guards against, so the numbers are generous rather than tuned.
const COMPACT_AT: usize = 65_536;
/// How many newest entries a compaction keeps.
const KEEP_ON_COMPACT: usize = 32_768;

/// The persisted set of already-delivered message ids.
pub struct SeenStore {
    path: PathBuf,
    seen: HashSet<[u8; 32]>,
    /// File order, for compaction. `seen` is the lookup, this is the age.
    order: Vec<[u8; 32]>,
}

impl SeenStore {
    /// Load `home/seen`. A missing file is an empty store; an unreadable
    /// line is skipped rather than fatal, because refusing to fetch over a
    /// damaged dedup record would trade duplicate mail for no mail.
    pub fn load(home: &Path) -> std::io::Result<Self> {
        let path = home.join(SEEN_FILE);
        let mut store = Self {
            path,
            seen: HashSet::new(),
            order: Vec::new(),
        };
        let text = match std::fs::read_to_string(&store.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(error) => return Err(error),
        };
        for line in text.lines() {
            if let Some(id) = parse_id(line.trim()) {
                if store.seen.insert(id) {
                    store.order.push(id);
                }
            }
        }
        if store.order.len() >= COMPACT_AT {
            store.compact()?;
        }
        Ok(store)
    }

    /// Whether this id has been delivered by an earlier run (or earlier in
    /// this one).
    pub fn contains(&self, id: &[u8; 32]) -> bool {
        self.seen.contains(id)
    }

    /// Record one delivered id. Returns `true` when it was new; a new id is
    /// appended to the file immediately, so a fetch that dies mid-run does
    /// not re-print what it already printed.
    pub fn record(&mut self, id: [u8; 32]) -> std::io::Result<bool> {
        if !self.seen.insert(id) {
            return Ok(false);
        }
        self.order.push(id);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", crate::address::to_hex(&id))?;
        Ok(true)
    }

    /// Rewrite the file to the newest [`KEEP_ON_COMPACT`] entries.
    fn compact(&mut self) -> std::io::Result<()> {
        let keep_from = self.order.len().saturating_sub(KEEP_ON_COMPACT);
        let kept: Vec<[u8; 32]> = self.order.split_off(keep_from);
        self.order = kept;
        self.seen = self.order.iter().copied().collect();
        let mut text = String::with_capacity(self.order.len() * 65);
        for id in &self.order {
            text.push_str(&crate::address::to_hex(id));
            text.push('\n');
        }
        // Write-then-rename, so a crash mid-compaction leaves the old file
        // rather than half of one.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)
    }
}

fn parse_id(line: &str) -> Option<[u8; 32]> {
    if line.len() != 64 || !line.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut id = [0u8; 32];
    for (byte, pair) in id.iter_mut().zip(line.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => c.to_ascii_lowercase() - b'a' + 10,
        };
        *byte = (digit(pair[0]) << 4) | digit(pair[1]);
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_id_survives_a_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = [0xab; 32];

        let mut store = SeenStore::load(dir.path()).expect("empty load");
        assert!(!store.contains(&id));
        assert!(store.record(id).expect("record"), "first sight is new");
        assert!(!store.record(id).expect("record"), "second sight is not");

        let reloaded = SeenStore::load(dir.path()).expect("reload");
        assert!(
            reloaded.contains(&id),
            "the whole point: the memory survives the process"
        );
    }

    #[test]
    fn a_damaged_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(SEEN_FILE),
            "not hex at all\n".to_string() + &"cd".repeat(32) + "\n",
        )
        .expect("write");
        let store = SeenStore::load(dir.path()).expect("damage is not fatal");
        assert!(store.contains(&[0xcd; 32]), "the intact line still counts");
    }

    #[test]
    fn compaction_keeps_the_newest_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut text = String::new();
        // COMPACT_AT distinct ids, oldest first.
        for n in 0..COMPACT_AT as u32 {
            let mut id = [0u8; 32];
            id[..4].copy_from_slice(&n.to_be_bytes());
            text.push_str(&crate::address::to_hex(&id));
            text.push('\n');
        }
        std::fs::write(dir.path().join(SEEN_FILE), text).expect("write");

        let store = SeenStore::load(dir.path()).expect("load compacts");
        let mut oldest = [0u8; 32];
        oldest[..4].copy_from_slice(&0u32.to_be_bytes());
        let mut newest = [0u8; 32];
        newest[..4].copy_from_slice(&(COMPACT_AT as u32 - 1).to_be_bytes());
        assert!(!store.contains(&oldest), "the oldest entries are dropped");
        assert!(store.contains(&newest), "the newest entries are kept");

        let on_disk = std::fs::read_to_string(dir.path().join(SEEN_FILE)).expect("read back");
        assert_eq!(
            on_disk.lines().count(),
            KEEP_ON_COMPACT,
            "the file itself is rewritten, not just the in-memory view"
        );
    }
}
