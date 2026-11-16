//! `loquax export <dir>`: loquax's identity, contacts and history in the
//! shape the ecosystem already shares, NomadNet's data tree with LXMF's own
//! `packed_container` per message (`docs/src/concepts/loquax-import.md`,
//! "The export"). A user can leave as easily as they came, and
//! `loquax import <dir>` reads it back through the NomadNet reader, which is
//! the round trip the tests hold.
//!
//! # The signed bytes
//!
//! The store keeps a message's text and fields, not the bytes it travelled
//! as, except while a received message waits on its sender's key. So:
//!
//! - a received message whose bytes are kept goes out as those bytes, and
//!   its signature checks wherever it is read;
//! - a message loquax sent is signed again with loquax's identity, over the
//!   sender time the store has (or the time it was written); its id is then
//!   not the id it travelled under, so the file keeps the old id as its name;
//! - any other received message goes out with an empty signature, which no
//!   reader can verify, and loquax's verdict on it in the container's
//!   [`crate::import::nomadnet::VERDICT_KEY`], which an import
//!   shows as loquax's word, never as its own.

use std::path::Path;

use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm};
use leviculum_lxmf::msgpack;
use leviculum_std::Identity;
use leviculum_ui::delivery::DeliveryState;

use crate::address::to_hex;
use crate::import::nomadnet::{state, VERDICT_KEY};
use crate::import::{key, ImportError};
use crate::store::{ExportRow, Store, STORE_FILE};
use crate::ui::Signature;

/// The file under `storage/` that says a tree is a loquax export.
pub const MARKER: &str = "loquax-export";

/// What an export wrote.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Exported {
    pub address: [u8; 16],
    pub messages: usize,
    pub conversations: usize,
    pub contacts: usize,
}

/// Write the export of the loquax home `home` into `dir`, which must not
/// exist or be empty: an export never writes over files.
pub fn export(home: &Path, dir: &Path) -> Result<Exported, ImportError> {
    if std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(ImportError::new(
            dir,
            "not empty; an export writes into a new directory",
        ));
    }
    let identity_path = home.join("identity");
    let bytes = std::fs::read(&identity_path).map_err(|e| ImportError::new(&identity_path, e))?;
    let identity = leviculum_core::identity_store::decode_identity(&bytes)
        .ok_or_else(|| ImportError::new(&identity_path, "not a readable identity record"))?;
    let raw = identity
        .private_key_bytes()
        .map_err(|e| ImportError::new(&identity_path, format!("{e:?}")))?;
    let address = key::address_of(&raw).map_err(|e| ImportError::new(&identity_path, e))?;
    let storage = dir.join("storage");
    let conversations = storage.join("conversations");
    std::fs::create_dir_all(&conversations).map_err(|e| ImportError::new(&conversations, e))?;
    key::write_private(&storage.join("identity"), &raw)
        .map_err(|e| ImportError::new(&storage, e))?;
    write(
        &storage.join(MARKER),
        b"loquax export: NomadNet's tree, LXMF packed_container per message\n",
    )?;
    // The delivery destination's ratchets, where NomadNet's router loads
    // them (`<storage>/lxmf/ratchets/<address>.ratchets`, LXMRouter.py:131
    // and :357; the file is already in that format): every peer that heard
    // Loquax announce encrypts short messages to one of them for 30 days,
    // and the program the identity moves to has to hold them (1185).
    let held = leviculum_std::dest_ratchet_keys_path(&home.join("storage"), &address);
    match std::fs::read(&held) {
        Ok(ratchets) => {
            let to = storage
                .join("lxmf/ratchets")
                .join(format!("{}.ratchets", to_hex(&address)));
            key::write_private(&to, &ratchets).map_err(|e| ImportError::new(&to, e))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ImportError::new(&held, error)),
    }

    let mut out = Exported {
        address,
        ..Exported::default()
    };
    let contacts = crate::names::load_contacts(home).map_err(|e| ImportError::new(home, e))?;
    out.contacts = contacts.petnames.len();
    write(&storage.join("directory"), &directory(&contacts))?;
    let ignored: String = contacts
        .blocked
        .iter()
        .map(|peer| format!("{}\n", to_hex(peer)))
        .collect();
    write(&dir.join("ignored"), ignored.as_bytes())?;
    if let Ok(config) = crate::config::load(home) {
        write(
            &storage.join("peersettings"),
            &peersettings(config.propagation_node),
        )?;
    }

    let store_error = |e: crate::store::StoreError| ImportError::new(&home.join(STORE_FILE), e);
    let Some(store) =
        Store::open_read_only(&home.join(STORE_FILE), address).map_err(store_error)?
    else {
        return Ok(out);
    };
    let rows = store.export_rows().map_err(store_error)?;
    let mut peers = std::collections::BTreeSet::new();
    for row in &rows {
        let (name, container) =
            container(row, &identity, address).map_err(|e| ImportError::new(&conversations, e))?;
        let peer_dir = conversations.join(to_hex(&row.peer));
        if peers.insert(row.peer) {
            std::fs::create_dir_all(&peer_dir).map_err(|e| ImportError::new(&peer_dir, e))?;
        }
        write(&peer_dir.join(to_hex(&name)), &container)?;
        out.messages += 1;
    }
    out.conversations = peers.len();
    Ok(out)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), ImportError> {
    std::fs::write(path, bytes).map_err(|e| ImportError::new(path, e))
}

/// The file name (the message's id) and the container of one message.
fn container(
    export: &ExportRow,
    identity: &Identity,
    address: [u8; 16],
) -> Result<([u8; 32], Vec<u8>), String> {
    let row = &export.row;
    let when = row
        .timestamp
        .or(row.received_at.map(|secs| secs as f64))
        .unwrap_or(0.0);
    let method = match row.delivery {
        Some(DeliveryState::InMailbox) => DeliveryMethod::Propagated,
        _ => DeliveryMethod::Direct,
    };
    let packed = match (&row.held, row.outbound) {
        (Some((_, packed)), false) => packed.clone(),
        (_, true) => Lxm::create(
            export.peer,
            address,
            identity,
            when,
            row.title.clone(),
            row.content.as_bytes().to_vec(),
            row.fields.clone(),
            method,
        )
        .map_err(|e| e.to_string())?
        .pack(),
        (None, false) => {
            // Signed by nobody: the layout of a message from the peer, with
            // a signature no key verifies.
            let mut message = Lxm::create(
                address,
                export.peer,
                identity,
                when,
                row.title.clone(),
                row.content.as_bytes().to_vec(),
                row.fields.clone(),
                method,
            )
            .map_err(|e| e.to_string())?;
            message.signature = [0; 64];
            message.pack()
        }
    };
    let id = match row.id == [0; 32] {
        true => {
            Lxm::unpack(&packed, None, None, DeliveryMethod::Direct)
                .map_err(|e| e.to_string())?
                .message_id
        }
        false => row.id,
    };
    let lxmf_state = match (row.outbound, row.delivery) {
        (false, _) => state::DELIVERED,
        (
            true,
            Some(DeliveryState::Arrived | DeliveryState::Delivered | DeliveryState::Read { .. }),
        ) => state::DELIVERED,
        (true, Some(DeliveryState::Sent | DeliveryState::InMailbox)) => state::SENT,
        (true, Some(DeliveryState::Rejected)) => state::REJECTED,
        (true, Some(DeliveryState::Failed)) => state::FAILED,
        (true, _) => state::OUTBOUND,
    };
    let verdict = match row.signature {
        Signature::Verified => 1,
        Signature::Unverified => 0,
        Signature::Invalid => 2,
    };
    let mut out = Vec::new();
    msgpack::map(&mut out, 6);
    msgpack::string(&mut out, "state");
    msgpack::int(&mut out, lxmf_state);
    msgpack::string(&mut out, "lxmf_bytes");
    msgpack::bin(&mut out, &packed);
    msgpack::string(&mut out, "transport_encrypted");
    msgpack::bool(&mut out, false);
    msgpack::string(&mut out, "transport_encryption");
    msgpack::nil(&mut out);
    msgpack::string(&mut out, "method");
    msgpack::int(&mut out, method as i64);
    msgpack::string(&mut out, VERDICT_KEY);
    msgpack::int(&mut out, verdict);
    Ok((id, out))
}

/// NomadNet's `directory`: one entry a named peer, trust UNKNOWN, no node.
fn directory(contacts: &crate::names::Contacts) -> Vec<u8> {
    /// `DirectoryEntry.UNKNOWN` (`Directory.py:413-416`).
    const UNKNOWN: i64 = 0x02;
    let mut out = Vec::new();
    msgpack::map(&mut out, 2);
    msgpack::string(&mut out, "entry_list");
    msgpack::array(&mut out, contacts.petnames.len());
    for (peer, name) in &contacts.petnames {
        msgpack::array(&mut out, 8);
        msgpack::bin(&mut out, peer);
        msgpack::string(&mut out, name);
        msgpack::int(&mut out, UNKNOWN);
        msgpack::bool(&mut out, false);
        msgpack::nil(&mut out);
        msgpack::bool(&mut out, false);
        msgpack::nil(&mut out);
        msgpack::string(&mut out, "");
    }
    msgpack::string(&mut out, "announce_stream");
    msgpack::array(&mut out, 0);
    out
}

/// NomadNet's `peersettings`, with what loquax has of it.
fn peersettings(propagation_node: Option<[u8; 16]>) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::map(&mut out, 1);
    msgpack::string(&mut out, "propagation_node");
    match propagation_node {
        Some(pn) => msgpack::bin(&mut out, &pn),
        None => msgpack::nil(&mut out),
    }
    out
}
