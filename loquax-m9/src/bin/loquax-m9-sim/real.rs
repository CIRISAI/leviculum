//! `--store real` (1530): the 1489 fixture (`loquax/tests/support/
//! real_store.rs`, 300 conversations, 4000 messages) as the archive a home
//! would hand the board, entry by entry: each message packed and signed as
//! LXMF, received ones to the reader's address, sent ones from it, the
//! names as name entries, and what the fixture has read as read marks. The
//! sim pulls it into a blank board page by page through the board's own
//! keep and fold ([`crate::board::Board::pull`]), so the store it leaves is
//! the store a first pull leaves.
//!
//! What the board's archive cannot carry of the fixture: the group's member
//! list (the home sends it as a group entry the fixture has no bytes for),
//! the pinned and archived flags and the mute (the phone's own state), the
//! delivery and read receipts of sent messages. The board shows those
//! conversations as the store holds them.

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::wire::{self, Entry, Mark, MarkFlag, MARK_IDS_MAX};
use loquax::home::{KIND_MARK, KIND_NAME, KIND_RECEIVED, KIND_SENT};

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../../../../loquax/tests/support/real_store.rs"]
pub mod real_store;

/// The address the fixture's messages are to and from: the home's.
#[allow(dead_code)]
pub fn address() -> [u8; 16] {
    real_store::own()
}

/// The fixture's archive, oldest first, its sequences from 1.
#[allow(dead_code)]
pub fn archive() -> Result<Vec<Entry>, String> {
    archive_for(real_store::own())
}

/// The fixture's archive for the home whose LXMF address is `own`
/// (1562: the bench home seeds its own archive with it, under its own
/// identity's address, so a board that joins it pulls the fixture).
pub fn archive_for(own: [u8; 16]) -> Result<Vec<Entry>, String> {
    archive_of(own, &real_store::restored(), &real_store::names())
}

/// The archive of `restored` with `names` for the home whose LXMF address
/// is `own` (1581: the list's draw measured over a part of the fixture, or
/// the fixture copied under other peers, by `examples/list_measure.rs`).
#[allow(dead_code)]
pub fn archive_of(
    own: [u8; 16],
    restored: &[loquax::ui::history::Restored],
    names: &[([u8; 16], String)],
) -> Result<Vec<Entry>, String> {
    // The home signs nothing the board checks (`board::fold` says
    // `verified: false`); one fixed key signs every message, so the bytes
    // are the same on every run.
    let signer = Identity::from_private_key_bytes(&[0x53; 64])
        .map_err(|e| format!("the fixture's signing key: {e:?}"))?;
    let mut rows: Vec<(i64, u8, Vec<u8>)> = Vec::new();
    for (peer, name) in names {
        rows.push((0, KIND_NAME, wire::name_entry(peer, name)));
    }
    let mut read: Vec<[u8; 32]> = Vec::new();
    for restored in restored {
        let received = restored.messages.iter().filter(|m| !m.outbound).count();
        let mut seen = 0;
        for message in &restored.messages {
            let at = message.at.map_or(0, |at| at.unix_secs);
            let (to, from, kind) = match message.outbound {
                true => (restored.peer, own, KIND_SENT),
                false => (own, restored.peer, KIND_RECEIVED),
            };
            let packed = Message::create(
                to,
                from,
                &signer,
                at as f64,
                Vec::new(),
                message.body.as_bytes().to_vec(),
                message.fields.clone(),
                DeliveryMethod::Direct,
            )
            .map_err(|e| format!("a fixture message: {e:?}"))?;
            if !message.outbound {
                seen += 1;
                // The unread are the newest received ones.
                if seen + restored.unread <= received {
                    read.push(packed.message_id);
                }
            }
            rows.push((at, kind, packed.pack()));
        }
    }
    // As a home's archive holds them: by the time it took them in.
    rows.sort_by_key(|(at, _, _)| *at);
    let newest = rows.last().map_or(0, |(at, _, _)| *at);
    for ids in read.chunks(MARK_IDS_MAX) {
        let mark = Mark {
            flag: MarkFlag::Read,
            ids: ids.to_vec(),
        };
        rows.push((newest, KIND_MARK, wire::mark_request(&mark)));
    }
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(i, (at, kind, packed))| Entry {
            seq: i as u64 + 1,
            kind,
            at,
            packed,
        })
        .collect())
}
