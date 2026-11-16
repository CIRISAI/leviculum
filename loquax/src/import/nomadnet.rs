//! NomadNet's data directory, read from its format
//! (`docs/drafts/loquax-import-2026-10.md`, "NomadNet (1.4.4)"), and the
//! loquax export, which writes the same tree (`crate::export`).
//!
//! ```text
//! <root>/storage/identity                     raw 64-byte identity
//! <root>/storage/directory                    msgpack {entry_list, announce_stream}
//! <root>/storage/peersettings                 msgpack {display_name, propagation_node, ...}
//! <root>/storage/conversations/<peer>/<id>    LXMF's packed_container per message
//! <root>/storage/conversations/<peer>/.index  msgpack {<id>: {signature_validated, sort_timestamp, ...}}
//! <root>/storage/attachments/<id>/            what NomadNet moved out of a message
//! <root>/ignored                              one blocked hash in hex a line
//! ```
//!
//! Every field is optional, as NomadNet's own readers treat them: the files
//! carry no version.

use std::path::{Path, PathBuf};

use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm, Verification};
use leviculum_std::Identity;
use leviculum_ui::delivery::DeliveryState;

use super::mpack::{self, Map};
use super::{key, ImportError};
use crate::address::{parse_id, to_hex};
use crate::names::{sanitise, Contact};
use crate::store::ImportRow;
use crate::ui::Signature;

/// The container key the loquax export adds for its own verdict on a
/// message whose signed bytes it no longer holds. NomadNet reads only the
/// keys it knows (`if key in container`), so the extra key costs it nothing.
pub const VERDICT_KEY: &str = "loquax_verification";

/// LXMF's message states (`LXMF/LXMessage.py`), as the container stores
/// them.
pub mod state {
    pub const OUTBOUND: i64 = 0x01;
    pub const SENT: i64 = 0x04;
    pub const DELIVERED: i64 = 0x08;
    pub const REJECTED: i64 = 0xFD;
    pub const FAILED: i64 = 0xFF;
}

/// One NomadNet tree.
pub struct Tree {
    root: PathBuf,
}

/// A conversation directory: the peer and its message files.
pub struct Conversation {
    pub peer: [u8; 16],
    pub dir: PathBuf,
    pub files: Vec<([u8; 32], PathBuf)>,
}

/// One conversation read whole.
#[derive(Default)]
pub struct ConversationRead {
    pub rows: Vec<ImportRow>,
    /// Files that are not a readable message, each with the reason.
    pub unreadable: Vec<(PathBuf, String)>,
    /// Attachments NomadNet moved out of these messages: not imported until
    /// loquax has an attachment store.
    pub attachments: usize,
}

/// The own settings NomadNet keeps.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub display_name: Option<String>,
    pub propagation_node: Option<[u8; 16]>,
}

impl Tree {
    /// The tree at `root`, when it has NomadNet's storage: an identity or a
    /// conversations directory.
    pub fn at(root: &Path) -> Option<Self> {
        let storage = root.join("storage");
        (storage.join("identity").is_file() || storage.join("conversations").is_dir()).then(|| {
            Self {
                root: root.to_path_buf(),
            }
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn storage(&self) -> PathBuf {
        self.root.join("storage")
    }

    /// The raw identity, when the tree has one.
    pub fn identity(&self) -> Result<Option<[u8; key::KEY_LEN]>, ImportError> {
        let path = self.storage().join("identity");
        match std::fs::read(&path) {
            Ok(bytes) => key::from_raw(&bytes)
                .map(Some)
                .ok_or_else(|| ImportError::new(&path, "not a 64-byte identity")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ImportError::new(&path, error)),
        }
    }

    /// The directory's named peers as petnames, and `ignored` as blocks.
    /// Node entries are keyed by their node hash, not an LXMF address, and
    /// are left out.
    pub fn contacts(&self) -> Result<Vec<Contact>, ImportError> {
        let mut out: Vec<Contact> = Vec::new();
        let path = self.storage().join("directory");
        match std::fs::read(&path) {
            Ok(bytes) => {
                let map = Map::read(&bytes).map_err(|e| ImportError::new(&path, e))?;
                let entries = map
                    .get("entry_list")
                    .and_then(mpack::array)
                    .unwrap_or_default();
                for entry in entries {
                    let Some(fields) = mpack::array(entry) else {
                        continue;
                    };
                    let Some(peer) = fields
                        .first()
                        .and_then(|v| mpack::bytes(v))
                        .and_then(|b| <[u8; 16]>::try_from(b).ok())
                    else {
                        continue;
                    };
                    let hosts_node = fields.get(3).and_then(|v| mpack::boolean(v));
                    if hosts_node == Some(true) {
                        continue;
                    }
                    let petname = fields
                        .get(1)
                        .and_then(|v| mpack::string(v))
                        .filter(|name| name != "Undefined")
                        .and_then(|name| sanitise(&name));
                    // The trust level (`Directory.py:413-416`) is not
                    // imported: loquax's trust is naming and blocking, and
                    // a NomadNet warning is not a block.
                    if petname.is_some() {
                        out.push(Contact {
                            peer,
                            petname,
                            blocked: false,
                        });
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ImportError::new(&path, error)),
        }
        let path = self.root.join("ignored");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for line in text.lines() {
                    let Ok(peer) = crate::address::parse(line.trim()) else {
                        continue;
                    };
                    match out.iter_mut().find(|c| c.peer == peer) {
                        Some(contact) => contact.blocked = true,
                        None => out.push(Contact {
                            peer,
                            petname: None,
                            blocked: true,
                        }),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ImportError::new(&path, error)),
        }
        Ok(out)
    }

    /// The own display name and propagation node, from `peersettings` or
    /// its backup.
    pub fn settings(&self) -> Settings {
        let path = self.storage().join("peersettings");
        let backup = self.storage().join("peersettings.bak");
        let Some(bytes) = std::fs::read(&path)
            .ok()
            .or_else(|| std::fs::read(&backup).ok())
        else {
            return Settings::default();
        };
        let Ok(map) = Map::read(&bytes) else {
            return Settings::default();
        };
        Settings {
            display_name: map
                .get("display_name")
                .and_then(mpack::string)
                .and_then(|name| sanitise(&name)),
            propagation_node: map
                .get("propagation_node")
                .and_then(mpack::bytes)
                .and_then(|b| <[u8; 16]>::try_from(b).ok()),
        }
    }

    /// The conversation directories and their message files, by name: no
    /// file is opened.
    pub fn conversations(&self) -> Result<Vec<Conversation>, ImportError> {
        let base = self.storage().join("conversations");
        let entries = match std::fs::read_dir(&base) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ImportError::new(&base, error)),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| ImportError::new(&base, e))?;
            let dir = entry.path();
            let Some(peer) = entry
                .file_name()
                .to_str()
                .and_then(|name| crate::address::parse(name).ok())
            else {
                continue;
            };
            if !dir.is_dir() {
                continue;
            }
            let mut files = Vec::new();
            for file in std::fs::read_dir(&dir).map_err(|e| ImportError::new(&dir, e))? {
                let file = file.map_err(|e| ImportError::new(&dir, e))?;
                let Some(id) = file.file_name().to_str().and_then(parse_id) else {
                    continue;
                };
                files.push((id, file.path()));
            }
            files.sort();
            out.push(Conversation { peer, dir, files });
        }
        out.sort_by_key(|c| c.peer);
        Ok(out)
    }

    /// The directory's peer entries, counted without reading a message.
    pub fn contact_count(&self) -> usize {
        self.contacts().map(|c| c.len()).unwrap_or(0)
    }

    /// One conversation's messages. `own` is the identity the history
    /// belongs to: a message is outbound iff it comes from `own_address`
    /// (`Conversations.py:2339`), and an outbound one's signature is checked
    /// against `own`.
    pub fn read(
        &self,
        conversation: &Conversation,
        own: Option<&Identity>,
        own_address: [u8; 16],
    ) -> ConversationRead {
        let index = read_index(&conversation.dir.join(".index"));
        let mut out = ConversationRead::default();
        for (id, path) in &conversation.files {
            let entry = index.iter().find(|(name, _)| name == id).map(|(_, e)| e);
            match read_message(path, *id, entry, own, own_address) {
                Ok(row) => {
                    out.attachments += self.attachments_of(id);
                    out.rows.push(row);
                }
                Err(reason) => out.unreadable.push((path.clone(), reason)),
            }
        }
        out
    }

    /// How many files NomadNet moved out of message `id`.
    fn attachments_of(&self, id: &[u8; 32]) -> usize {
        let dir = self.storage().join("attachments").join(to_hex(id));
        let Ok(manifest) = std::fs::read(dir.join("manifest")) else {
            return usize::from(dir.is_dir());
        };
        Map::read(&manifest)
            .ok()
            .and_then(|map| map.get("files").and_then(mpack::array))
            .map_or(1, |files| files.len())
    }
}

/// What NomadNet's `.index` says of one message.
#[derive(Debug, Default, Clone, PartialEq)]
struct IndexEntry {
    signature_validated: Option<bool>,
    /// The file's modification time when NomadNet indexed it: when it wrote
    /// the message down.
    sort_timestamp: Option<f64>,
}

fn read_index(path: &Path) -> Vec<([u8; 32], IndexEntry)> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(map) = Map::read(&bytes) else {
        return Vec::new();
    };
    map.entries()
        .filter_map(|(name, value)| {
            let id = parse_id(std::str::from_utf8(name).ok()?)?;
            let entry = Map::read(value).ok()?;
            Some((
                id,
                IndexEntry {
                    signature_validated: entry.get("signature_validated").and_then(mpack::boolean),
                    sort_timestamp: entry.get("sort_timestamp").and_then(mpack::number),
                },
            ))
        })
        .collect()
}

/// One message file as an import row; the reason when it is not one.
///
/// The verdict rule: loquax's own verdict comes only from checking the
/// signature itself. The bytes are intact when the id they hash to is the
/// id the file is named by; NomadNet rewrites a message without its
/// attachments and keeps the name, so a mismatch is a changed message, whose
/// signature cannot hold, and it is stored unverified with the source's
/// verdict as the source's, never as invalid. Intact bytes from a sender
/// whose key is not known are kept for the check when the key comes.
fn read_message(
    path: &Path,
    id: [u8; 32],
    index: Option<&IndexEntry>,
    own: Option<&Identity>,
    own_address: [u8; 16],
) -> Result<ImportRow, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let container = Map::read(&bytes).map_err(|e| format!("not a message container: {e}"))?;
    let packed = container
        .get("lxmf_bytes")
        .and_then(mpack::bytes)
        .ok_or("the container has no lxmf_bytes")?;
    let parsed =
        Lxm::unpack(packed, None, None, DeliveryMethod::Direct).map_err(|e| e.to_string())?;
    let outbound = parsed.source_hash == own_address;
    if !outbound && parsed.destination_hash != own_address {
        return Err("neither from nor to this identity".to_string());
    }
    let intact = parsed.message_id == id;
    let checked = match (outbound, own) {
        (true, Some(own)) => Lxm::unpack(packed, None, Some(own), DeliveryMethod::Direct)
            .map(|m| m.verification)
            .unwrap_or(Verification::Unverified),
        _ => Verification::Unverified,
    };
    let signature = match (checked, intact) {
        (Verification::Valid, _) => Signature::Verified,
        (Verification::Invalid, true) => Signature::Invalid,
        _ => Signature::Unverified,
    };
    let held = (!outbound && intact).then(|| (parsed.source_hash, packed.to_vec()));
    let origin_verdict = container
        .get(VERDICT_KEY)
        .and_then(mpack::int)
        .map(|code| code == 1)
        .or(index.and_then(|e| e.signature_validated));
    let received_at = index
        .and_then(|e| e.sort_timestamp)
        .map(|secs| secs as i64)
        .or_else(|| modified(path));
    let state = container.get("state").and_then(mpack::int);
    let method = container.get("method").and_then(mpack::int);
    Ok(ImportRow {
        id,
        outbound,
        timestamp: Some(parsed.timestamp),
        received_at,
        title: parsed.title.clone(),
        content: String::from_utf8_lossy(&parsed.content).into_owned(),
        fields: parsed.fields.clone(),
        delivery: outbound.then(|| delivery_of(state, method)),
        signature,
        origin_verdict,
        held,
    })
}

/// A sent message's LXMF state as loquax's. One the source never got off
/// the device is failed: loquax does not send it again on its behalf.
fn delivery_of(state: Option<i64>, method: Option<i64>) -> DeliveryState {
    match state {
        // A proof, which the recipient's mailbox may have signed: one tick
        // (1462).
        Some(state::DELIVERED) => DeliveryState::Arrived,
        Some(state::SENT) if method == Some(DeliveryMethod::Propagated as i64) => {
            DeliveryState::InMailbox
        }
        Some(state::SENT) => DeliveryState::Sent,
        Some(state::REJECTED) => DeliveryState::Rejected,
        _ => DeliveryState::Failed,
    }
}

fn modified(path: &Path) -> Option<i64> {
    let time = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = time.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_is_64_hex_characters() {
        assert_eq!(parse_id(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(parse_id(".index"), None);
        assert_eq!(parse_id(&"zz".repeat(32)), None);
        assert_eq!(parse_id(&format!("{}.tmp.1", "ab".repeat(32))), None);
    }

    #[test]
    fn states_map_to_loquax_delivery() {
        assert_eq!(
            delivery_of(Some(state::DELIVERED), Some(2)),
            DeliveryState::Arrived
        );
        assert_eq!(
            delivery_of(Some(state::SENT), Some(3)),
            DeliveryState::InMailbox
        );
        assert_eq!(delivery_of(Some(state::SENT), Some(1)), DeliveryState::Sent);
        assert_eq!(
            delivery_of(Some(state::OUTBOUND), Some(2)),
            DeliveryState::Failed
        );
        assert_eq!(delivery_of(None, None), DeliveryState::Failed);
    }
}
