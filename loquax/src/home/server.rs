//! How the home node answers its clients: a pure function of the request,
//! the identity the link was identified with, and the store. The daemon
//! (`super::daemon`) carries the bytes; everything decided is here, where a
//! test reaches it without a network.
//!
//! Membership is checked on every request, as lxmd's control handlers check
//! their allow list on each call (`reference/LXMF/LXMF/LXMRouter.py:838-865`):
//! a device revoked while its link is up is refused on its next request.

use alloc::vec::Vec;

use leviculum_core::Identity;
use leviculum_lxmf::DeliveryMethod;

use super::wire::{self, Status};
use crate::store::{Accepted, Outgoing, Redeemed, Store};

/// What the home node answers with besides the store: the identity that
/// signs what clients send, and the configured limits.
pub struct Home<'a> {
    /// The home identity, with its private key.
    pub identity: &'a Identity,
    /// Its LXMF delivery address, every sent message's source.
    pub address: [u8; 16],
    /// How long an admission token stays good, in seconds.
    pub token_ttl_secs: i64,
    /// The archive's cap in bytes.
    pub cap_bytes: u64,
    /// The move its owner opened (1281), when one is open: what `/move`
    /// and `/moved` are checked against and carry.
    pub handover: Option<Handover>,
}

/// An open move (1281, [`super::handover`]): the token the new device
/// joined with, and what goes along besides the identity and the archive:
/// the delivery destination's ratchets and the node's known identities as
/// the node keeps them, and the name the mailbox announces.
pub struct Handover {
    pub token: [u8; 16],
    pub ratchets: Vec<u8>,
    pub known: Vec<u8>,
    pub name: alloc::string::String,
}

/// One answer: the bytes for the client, the messages a `/send` or a seen
/// `/mark` put in the outbox, the new ones, for the daemon to start
/// delivering, and for a `/watch` it took, the newest sequence the client was told, so
/// the daemon keeps the link for its wakes, and for a `/key` it took, the
/// peer and the public key for the daemon to remember.
pub struct Answer {
    pub bytes: Vec<u8>,
    pub accepted: Vec<Outgoing>,
    pub watching: Option<u64>,
    pub learned: Option<([u8; 16], [u8; 64])>,
    /// A `/moved` took: the identity is on the new device now, and this
    /// mailbox ends once the answer is out (1281).
    pub gone: bool,
}

impl Answer {
    fn only(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            accepted: Vec::new(),
            watching: None,
            learned: None,
            gone: false,
        }
    }
}

/// The answer to one request on the home destination. `remote` is the
/// identity hash the link was identified with, if it was; `now` is the unix
/// time in seconds.
pub fn answer(
    store: &mut Store,
    home: &Home<'_>,
    path: &str,
    data: &[u8],
    remote: Option<[u8; 16]>,
    now: f64,
) -> Answer {
    let Some(device) = remote else {
        return Answer::only(wire::status_answer(Status::NotIdentified));
    };
    let now_secs = now as i64;
    match path {
        super::JOIN_PATH => Answer::only(join(store, home, data, device, now_secs)),
        super::SYNC_PATH => Answer::only(sync(store, data, device)),
        super::SEND_PATH => send(store, home, data, device, now),
        super::MARK_PATH => mark(store, home, data, device, now),
        super::PETNAME_PATH => Answer::only(petname(store, home, data, device, now_secs)),
        super::PEERSTATE_PATH => Answer::only(peer_state(store, home, data, device, now_secs)),
        super::WATCH_PATH => watch(store, data, device),
        super::KEY_PATH => key(store, data, device),
        super::MOVE_PATH => Answer::only(move_page(store, home, data, device)),
        super::MOVED_PATH => moved(store, home, data, device),
        _ => Answer::only(wire::status_answer(Status::Malformed)),
    }
}

/// Whether `device` may ask: an answer to refuse it with, when not.
fn refuse_unless_admitted(store: &Store, device: &[u8; 16], what: &str) -> Option<Vec<u8>> {
    match store.is_admitted(device) {
        Ok(true) => None,
        Ok(false) => {
            tracing::info!(
                "HOME {what} refused device={} reason=not-admitted",
                crate::address::to_hex(device)
            );
            Some(wire::status_answer(Status::NotAdmitted))
        }
        Err(error) => {
            tracing::warn!("HOME {what}: {error}");
            Some(wire::status_answer(Status::Failed))
        }
    }
}

fn send(store: &mut Store, home: &Home<'_>, data: &[u8], device: [u8; 16], now: f64) -> Answer {
    let Ok(submission) = wire::read_send_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "send") {
        return Answer::only(refused);
    }
    // A retried submission is answered from the store, before anything is
    // signed: the message it made then is the one it gets.
    match store.outbox_by_client(&submission.client_id) {
        Ok(Some(id)) => {
            tracing::info!(
                "HOME send again device={} message={}",
                crate::address::to_hex(&device),
                crate::address::to_hex(&id)
            );
            return Answer::only(wire::send_answer(&id));
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!("HOME send: {error}");
            return Answer::only(wire::status_answer(Status::Failed));
        }
    }
    let message = match leviculum_lxmf::Message::create(
        submission.destination,
        home.address,
        home.identity,
        now,
        submission.title,
        submission.content,
        submission.fields,
        DeliveryMethod::Direct,
    ) {
        Ok(message) => message,
        Err(error) => {
            tracing::info!("HOME send refused: {error}");
            return Answer::only(wire::status_answer(Status::Malformed));
        }
    };
    let packed = message.pack();
    let at = now as i64;
    match store.outbox_accept(
        &submission.client_id,
        &message.message_id,
        &submission.destination,
        &packed,
        at,
        home.cap_bytes,
    ) {
        Ok(Accepted::New { .. }) => {
            tracing::info!(
                "HOME send accepted device={} message={} to={}",
                crate::address::to_hex(&device),
                crate::address::to_hex(&message.message_id),
                crate::address::to_hex(&submission.destination)
            );
            Answer {
                bytes: wire::send_answer(&message.message_id),
                accepted: alloc::vec![Outgoing {
                    id: message.message_id,
                    destination: submission.destination,
                    packed,
                    created_at: at,
                    propagated: false,
                }],
                watching: None,
                learned: None,
                gone: false,
            }
        }
        Ok(Accepted::Again { id }) => Answer::only(wire::send_answer(&id)),
        Err(error) => {
            tracing::warn!("HOME send: {error}");
            Answer::only(wire::status_answer(Status::Failed))
        }
    }
}

/// A client's mark, kept as an archive entry under the hash of its bytes:
/// the same mark handed in again, from this client or another, is one
/// entry. A seen mark is kept as the part no device said before, and makes
/// the read receipts ([`seen`]).
fn mark(store: &mut Store, home: &Home<'_>, data: &[u8], device: [u8; 16], now: f64) -> Answer {
    let Ok(mark) = wire::read_mark(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "mark") {
        return Answer::only(refused);
    }
    if let wire::MarkFlag::Seen { at } = mark.flag {
        return seen(store, home, &mark.ids, at, device, now);
    }
    Answer::only(keep_mark(store, home, &mark, data, device, now as i64))
}

/// Append a mark's bytes as its archive entry: the status to answer.
fn keep_mark(
    store: &mut Store,
    home: &Home<'_>,
    mark: &wire::Mark,
    data: &[u8],
    device: [u8; 16],
    now: i64,
) -> Vec<u8> {
    let id = leviculum_core::crypto::sha256(data);
    match store.archive_append(super::KIND_MARK, &id, data, now, home.cap_bytes) {
        Ok(seq) => {
            tracing::info!(
                "HOME mark {} device={} messages={} seq={}",
                mark.flag.word(),
                crate::address::to_hex(&device),
                mark.ids.len(),
                seq.map_or_else(|| "-".to_string(), |seq| seq.to_string())
            );
            wire::status_answer(Status::Ok)
        }
        Err(error) => {
            tracing::warn!("HOME mark: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

/// A device saw these messages at the reader's clock `at` (1234): the
/// home node holds the identity, so it signs the read receipts. The first
/// device to see a message wins: an id a seen mark in the archive names
/// already is passed over, and a mark with nothing new appends nothing and
/// sends nothing. The rest is kept as one seen mark, which every device
/// applies on its next pull, and each sender whose message carried the
/// `lev` mark gets a receipt alone, signed here and delivered from the
/// outbox as every message a client hands in, direct first and then by the
/// propagation node. A message the archive does not hold as received (an
/// imported one, or one the cap evicted) is seen and gets none.
fn seen(
    store: &mut Store,
    home: &Home<'_>,
    ids: &[[u8; 32]],
    at: u32,
    device: [u8; 16],
    now: f64,
) -> Answer {
    let mut new = Vec::new();
    let mut owed: alloc::collections::BTreeMap<[u8; 16], Vec<[u8; 32]>> = Default::default();
    for id in ids {
        let (seen, entry) = match (
            store.archive_seen(id),
            store.archive_entry(super::KIND_RECEIVED, id),
        ) {
            (Ok(seen), Ok(entry)) => (seen, entry),
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!("HOME mark seen: {error}");
                return Answer::only(wire::status_answer(Status::Failed));
            }
        };
        if seen || new.contains(id) {
            continue;
        }
        new.push(*id);
        let Some(message) = entry.and_then(|packed| {
            leviculum_lxmf::Message::unpack(&packed, None, None, DeliveryMethod::Direct).ok()
        }) else {
            continue;
        };
        let marked = leviculum_lxmf::lev::Lev::from_fields(&message.fields)
            .is_some_and(|lev| lev.supports_receipts());
        if marked {
            owed.entry(message.source_hash).or_default().push(*id);
        }
    }
    if new.is_empty() {
        tracing::info!(
            "HOME mark seen device={} messages={} new=0",
            crate::address::to_hex(&device),
            ids.len()
        );
        return Answer::only(wire::status_answer(Status::Ok));
    }
    let kept = wire::Mark {
        flag: wire::MarkFlag::Seen { at },
        ids: new,
    };
    let status = keep_mark(
        store,
        home,
        &kept,
        &wire::mark_request(&kept),
        device,
        now as i64,
    );
    if wire::read_status(&status) != Ok(Status::Ok) {
        return Answer::only(status);
    }
    let mut accepted = Vec::new();
    for (peer, ids) in owed {
        for receipt in leviculum_lxmf::lev::Receipt::batches(at, &ids) {
            let count = receipt.ids.len();
            match receipt_out(store, home, peer, receipt, now) {
                Ok(Some(outgoing)) => {
                    tracing::info!(
                        "HOME receipt to={} ids={count} read_at={at} message={}",
                        crate::address::to_hex(&peer),
                        crate::address::to_hex(&outgoing.id)
                    );
                    accepted.push(outgoing);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!("HOME receipt to {}: {error}", crate::address::to_hex(&peer))
                }
            }
        }
    }
    Answer {
        bytes: status,
        accepted,
        watching: None,
        learned: None,
        gone: false,
    }
}

/// Sign one receipt alone to `peer` and put it in the outbox under its own
/// id: the new outbox entry, `None` when the outbox held it already.
fn receipt_out(
    store: &mut Store,
    home: &Home<'_>,
    peer: [u8; 16],
    receipt: leviculum_lxmf::lev::Receipt,
    now: f64,
) -> Result<Option<Outgoing>, alloc::string::String> {
    let message = leviculum_lxmf::Message::create(
        peer,
        home.address,
        home.identity,
        now,
        Vec::new(),
        Vec::new(),
        leviculum_lxmf::lev::Lev::mark_with(receipt)
            .fields()
            .to_vec(),
        DeliveryMethod::Direct,
    )
    .map_err(|error| error.to_string())?;
    let packed = message.pack();
    let at = now as i64;
    let mut client_id = [0; 16];
    client_id.copy_from_slice(&message.message_id[..16]);
    match store
        .outbox_accept(
            &client_id,
            &message.message_id,
            &peer,
            &packed,
            at,
            home.cap_bytes,
        )
        .map_err(|error| error.to_string())?
    {
        Accepted::New { .. } => Ok(Some(Outgoing {
            id: message.message_id,
            destination: peer,
            packed,
            created_at: at,
            propagated: false,
        })),
        Accepted::Again { .. } => Ok(None),
    }
}

/// A client's petname for a peer, kept as an archive entry: the newest of
/// each peer, the same one again appending nothing.
fn petname(store: &mut Store, home: &Home<'_>, data: &[u8], device: [u8; 16], now: i64) -> Vec<u8> {
    let Ok((peer, _)) = wire::read_petname_entry(data) else {
        return wire::status_answer(Status::Malformed);
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "petname") {
        return refused;
    }
    match store.archive_petname(peer, data, now, home.cap_bytes) {
        Ok(seq) => {
            tracing::info!(
                "HOME petname peer={} device={} seq={}",
                crate::address::to_hex(&peer),
                crate::address::to_hex(&device),
                seq.map_or_else(|| "-".to_string(), |seq| seq.to_string())
            );
            wire::status_answer(Status::Ok)
        }
        Err(error) => {
            tracing::warn!("HOME petname: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

/// Names the home node itself holds for peers, kept as petname entries the
/// way a client's `/petname` is (1159): an import's contacts and the
/// contacts file of a home node that imported before names travelled. A
/// peer the archive already holds a petname entry for, cleared or not,
/// keeps it: the reader's word on that peer was said. How many were
/// appended.
pub fn archive_petnames<'a>(
    store: &mut Store,
    names: impl IntoIterator<Item = (&'a [u8; 16], &'a str)>,
    now: i64,
    cap_bytes: u64,
) -> Result<usize, crate::store::StoreError> {
    let mut appended = 0;
    for (peer, name) in names {
        if name.is_empty() || store.archive_holds_petname(peer)? {
            continue;
        }
        let entry = wire::petname_entry(peer, Some(name));
        if store
            .archive_petname(*peer, &entry, now, cap_bytes)?
            .is_some()
        {
            appended += 1;
        }
    }
    Ok(appended)
}

/// Keep each of `peers` as blocked, a peer state entry
/// ([`crate::home::KIND_PEERSTATE`]) in the bytes a device's block sends
/// (1162): an import's blocks and the contacts file of a home node that
/// imported before blocks travelled. A peer the archive holds a peer state
/// for keeps it: a device said its word on that peer. How many were
/// appended.
pub fn archive_blocks<'a>(
    store: &mut Store,
    peers: impl IntoIterator<Item = &'a [u8; 16]>,
    now: i64,
    cap_bytes: u64,
) -> Result<usize, crate::store::StoreError> {
    let blocked = wire::PeerState {
        blocked: true,
        ..wire::PeerState::default()
    };
    let mut appended = 0;
    for peer in peers {
        if store.archive_holds_peer_state(peer)? {
            continue;
        }
        let entry = wire::peer_state_entry(peer, &blocked);
        if store
            .archive_peer_state(*peer, &entry, now, cap_bytes)?
            .is_some()
        {
            appended += 1;
        }
    }
    Ok(appended)
}

/// A client's word on a peer (1146), kept as an archive entry as a
/// petname is: the newest of each peer, the same one again appending
/// nothing.
fn peer_state(
    store: &mut Store,
    home: &Home<'_>,
    data: &[u8],
    device: [u8; 16],
    now: i64,
) -> Vec<u8> {
    let Ok((peer, _)) = wire::read_peer_state_entry(data) else {
        return wire::status_answer(Status::Malformed);
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "peerstate") {
        return refused;
    }
    match store.archive_peer_state(peer, data, now, home.cap_bytes) {
        Ok(seq) => {
            tracing::info!(
                "HOME peerstate peer={} device={} seq={}",
                crate::address::to_hex(&peer),
                crate::address::to_hex(&device),
                seq.map_or_else(|| "-".to_string(), |seq| seq.to_string())
            );
            wire::status_answer(Status::Ok)
        }
        Err(error) => {
            tracing::warn!("HOME peerstate: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

fn join(
    store: &mut Store,
    home: &Home<'_>,
    data: &[u8],
    device: [u8; 16],
    now_secs: i64,
) -> Vec<u8> {
    let Ok((token, brought)) = wire::read_join_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    match store.redeem_token_as(&token, &device, &brought, now_secs, home.token_ttl_secs) {
        Ok(Redeemed::Admitted { name }) => {
            tracing::info!(
                "HOME admitted device={} name={name:?}",
                crate::address::to_hex(&device)
            );
            wire::status_answer(Status::Ok)
        }
        Ok(Redeemed::Unknown) => {
            tracing::info!(
                "HOME join refused device={} reason=token",
                crate::address::to_hex(&device)
            );
            wire::status_answer(Status::BadToken)
        }
        Ok(Redeemed::Expired { name }) => {
            tracing::info!(
                "HOME join refused device={} reason=expired name={name:?}",
                crate::address::to_hex(&device)
            );
            wire::status_answer(Status::BadToken)
        }
        Err(error) => {
            tracing::warn!("HOME join: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

/// A client keeps its link for wakes: told the newest sequence now, so it
/// pulls at once when its cursor is behind.
fn watch(store: &mut Store, data: &[u8], device: [u8; 16]) -> Answer {
    let Ok(cursor) = wire::read_watch_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "watch") {
        return Answer::only(refused);
    }
    match store.archive_newest() {
        Ok(newest) => {
            tracing::info!(
                "HOME watch device={} cursor={cursor} newest={newest}",
                crate::address::to_hex(&device)
            );
            Answer {
                bytes: wire::watch_answer(newest),
                accepted: Vec::new(),
                watching: Some(newest),
                learned: None,
                gone: false,
            }
        }
        Err(error) => {
            tracing::warn!("HOME watch: {error}");
            Answer::only(wire::status_answer(Status::Failed))
        }
    }
}

/// A contact's key from its contact code (1187): taken when its address is
/// the one the key makes, for the daemon to remember; a pair that does not
/// belong together is malformed.
fn key(store: &mut Store, data: &[u8], device: [u8; 16]) -> Answer {
    let Ok((peer, public_key)) = wire::read_key_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "key") {
        return Answer::only(refused);
    }
    if super::lxmf_address(&public_key).ok() != Some(peer) {
        tracing::info!(
            "HOME key refused device={} peer={} reason=mismatch",
            crate::address::to_hex(&device),
            crate::address::to_hex(&peer)
        );
        return Answer::only(wire::status_answer(Status::Malformed));
    }
    tracing::info!(
        "HOME key device={} peer={}",
        crate::address::to_hex(&device),
        crate::address::to_hex(&peer)
    );
    Answer {
        bytes: wire::status_answer(Status::Ok),
        accepted: Vec::new(),
        watching: None,
        learned: Some((peer, public_key)),
        gone: false,
    }
}

/// Whether `device` may move the mailbox with `token` (1281): admitted, and
/// the token is the open move's. The handover back when it may, an answer
/// to refuse it with when not.
fn may_move<'h>(
    store: &Store,
    home: &'h Home<'_>,
    token: &[u8; 16],
    device: &[u8; 16],
    what: &str,
) -> Result<&'h Handover, Vec<u8>> {
    if let Some(refused) = refuse_unless_admitted(store, device, what) {
        return Err(refused);
    }
    match &home.handover {
        Some(handover) if handover.token == *token => Ok(handover),
        _ => {
            tracing::info!(
                "HOME {what} refused device={} reason=no-move",
                crate::address::to_hex(device)
            );
            Err(wire::status_answer(Status::BadToken))
        }
    }
}

/// A page of the move (1281): the identity, its ratchets and the archive
/// after the cursor with each entry's id.
fn move_page(store: &mut Store, home: &Home<'_>, data: &[u8], device: [u8; 16]) -> Vec<u8> {
    let Ok((token, cursor)) = wire::read_move_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    let handover = match may_move(store, home, &token, &device, "move") {
        Ok(handover) => handover,
        Err(refused) => return refused,
    };
    let Ok(private_key) = home.identity.private_key_bytes() else {
        return wire::status_answer(Status::Failed);
    };
    match store.archive_moving(cursor, super::SYNC_PAGE_BYTES) {
        Ok((entries, more, first)) => {
            tracing::info!(
                "HOME move device={} cursor={cursor} entries={} more={more}",
                crate::address::to_hex(&device),
                entries.len()
            );
            wire::move_answer(&wire::MovePage {
                private_key: private_key.to_vec(),
                ratchets: handover.ratchets.clone(),
                known: handover.known.clone(),
                name: handover.name.clone(),
                first,
                more,
                entries,
            })
        }
        Err(error) => {
            tracing::warn!("HOME move: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

/// The new device holds everything up to its cursor (1281): the mailbox
/// here is gone once this is answered, unless the archive grew past it.
fn moved(store: &mut Store, home: &Home<'_>, data: &[u8], device: [u8; 16]) -> Answer {
    let Ok((token, cursor)) = wire::read_move_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Err(refused) = may_move(store, home, &token, &device, "moved") {
        return Answer::only(refused);
    }
    match store.archive_newest() {
        Ok(newest) if newest > cursor => {
            tracing::info!(
                "HOME moved refused device={} cursor={cursor} newest={newest} reason=behind",
                crate::address::to_hex(&device)
            );
            Answer::only(wire::status_answer(Status::Behind))
        }
        Ok(_) => {
            tracing::warn!(
                "HOME moved device={} cursor={cursor}: the mailbox is on that device now",
                crate::address::to_hex(&device)
            );
            Answer {
                gone: true,
                ..Answer::only(wire::status_answer(Status::Ok))
            }
        }
        Err(error) => {
            tracing::warn!("HOME moved: {error}");
            Answer::only(wire::status_answer(Status::Failed))
        }
    }
}

fn sync(store: &mut Store, data: &[u8], device: [u8; 16]) -> Vec<u8> {
    let Ok(cursor) = wire::read_sync_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "sync") {
        return refused;
    }
    match store.archive_since(cursor, super::SYNC_PAGE_BYTES) {
        Ok(page) => {
            tracing::info!(
                "HOME sync device={} cursor={cursor} entries={} more={}",
                crate::address::to_hex(&device),
                page.entries.len(),
                page.more
            );
            wire::sync_answer(&page)
        }
        Err(error) => {
            tracing::warn!("HOME sync: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::outgoing::Change;
    use crate::home::wire::{read_status, read_sync_answer, sync_request, Page};
    use crate::home::{JOIN_PATH, KIND_RECEIVED, SEND_PATH, SYNC_PATH};

    const HOME: [u8; 16] = [0x11; 16];
    const LAPTOP: [u8; 16] = [0x22; 16];
    const TOKEN: [u8; 16] = [0x33; 16];

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("dir");
        let store = Store::open_in(dir.path(), HOME).expect("store");
        (dir, store)
    }

    const DAY: i64 = 24 * 3600;

    /// The home node's side, with a fresh identity each test.
    fn home() -> Home<'static> {
        let identity: &'static Identity = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            Identity::generate(&mut rand_core::OsRng),
        ));
        Home {
            identity,
            address: HOME,
            token_ttl_secs: DAY,
            cap_bytes: u64::MAX,
            handover: None,
        }
    }

    fn join_at(store: &mut Store, device: [u8; 16], token: [u8; 16], now: f64) -> Status {
        let answer = answer(
            store,
            &home(),
            JOIN_PATH,
            &wire::join_request(&token, "laptop"),
            Some(device),
            now,
        );
        read_status(&answer.bytes).expect("status")
    }

    fn join(store: &mut Store, device: [u8; 16], token: [u8; 16]) -> Status {
        join_at(store, device, token, 100.0)
    }

    fn sync(store: &mut Store, device: Option<[u8; 16]>, cursor: u64) -> Result<Page, Status> {
        let answer = answer(
            store,
            &home(),
            SYNC_PATH,
            &sync_request(cursor),
            device,
            100.0,
        );
        read_sync_answer(&answer.bytes).expect("answer")
    }

    fn submission(client_id: u8) -> wire::Submission {
        wire::Submission {
            client_id: [client_id; 16],
            destination: [0x66; 16],
            title: Vec::new(),
            content: b"hallo".to_vec(),
            fields: Vec::new(),
        }
    }

    fn send(
        store: &mut Store,
        home: &Home<'_>,
        device: [u8; 16],
        submission: &wire::Submission,
        now: f64,
    ) -> (Result<[u8; 32], Status>, Option<Outgoing>) {
        let answer = answer(
            store,
            home,
            SEND_PATH,
            &wire::send_request(submission),
            Some(device),
            now,
        );
        (
            wire::read_send_answer(&answer.bytes).expect("answer"),
            answer.accepted.into_iter().next(),
        )
    }

    fn admitted(store: &mut Store) {
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(store, LAPTOP, TOKEN), Status::Ok);
    }

    #[test]
    fn a_send_is_idempotent_by_the_clients_id() {
        let (_dir, mut store) = store();
        admitted(&mut store);
        let home = home();
        let (first, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 200.5);
        let id = first.expect("accepted");
        let accepted = accepted.expect("a new outbox entry");
        assert_eq!(accepted.id, id);
        assert_eq!(accepted.destination, [0x66; 16]);
        // The packed message is signed by the home identity, from its address.
        let message = leviculum_lxmf::Message::unpack(
            &accepted.packed,
            None,
            Some(home.identity),
            DeliveryMethod::Direct,
        )
        .expect("unpack");
        assert_eq!(message.message_id, id);
        assert_eq!(message.source_hash, HOME);
        assert_eq!(message.verification, leviculum_lxmf::Verification::Valid);
        assert_eq!(message.content, b"hallo");
        // The same client id again, later: the same message, nothing new.
        let (again, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 260.0);
        assert_eq!(again, Ok(id));
        assert!(accepted.is_none());
        // Another id is another message.
        let (other, accepted) = send(&mut store, &home, LAPTOP, &submission(2), 260.0);
        assert_ne!(other.expect("accepted"), id);
        assert!(accepted.is_some());
        // The outbox holds both, and the archive has one sent entry each.
        assert_eq!(store.outbox_open().expect("open").len(), 2);
        let page = store.archive_since(0, usize::MAX).expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [crate::home::KIND_SENT, crate::home::KIND_SENT]
        );
        assert_eq!(page.entries[0].packed, message.pack());
    }

    #[test]
    fn a_send_from_a_device_not_admitted_is_refused() {
        let (_dir, mut store) = store();
        let home = home();
        let (refused, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 200.0);
        assert_eq!(refused, Err(Status::NotAdmitted));
        assert!(accepted.is_none());
        assert!(store.outbox_open().expect("open").is_empty());
        let answer = answer(
            &mut store,
            &home,
            SEND_PATH,
            &wire::send_request(&submission(1)),
            None,
            1.0,
        );
        assert_eq!(read_status(&answer.bytes), Ok(Status::NotIdentified));
    }

    #[test]
    fn the_outbox_and_its_states_survive_a_restart() {
        let dir = tempfile::tempdir().expect("dir");
        let home = home();
        let id = {
            let mut store = Store::open_in(dir.path(), HOME).expect("store");
            admitted(&mut store);
            let (one, _) = send(&mut store, &home, LAPTOP, &submission(1), 200.0);
            let (two, _) = send(&mut store, &home, LAPTOP, &submission(2), 201.0);
            let (one, two) = (one.expect("one"), two.expect("two"));
            store
                .outbox_record(&one, Change::Propagated, 300, u64::MAX)
                .expect("record");
            store
                .outbox_record(&two, Change::Delivered, 301, u64::MAX)
                .expect("record");
            one
        };
        let mut store = Store::open_in(dir.path(), HOME).expect("reopen");
        let open = store.outbox_open().expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert!(open[0].propagated);
        assert_eq!(open[0].created_at, 200);
        // The retried submission still finds it after the restart.
        let (again, accepted) = send(&mut store, &home, LAPTOP, &submission(1), 400.0);
        assert_eq!(again, Ok(id));
        assert!(accepted.is_none());
        let kinds: Vec<u8> = store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                crate::home::KIND_SENT,
                crate::home::KIND_SENT,
                crate::home::KIND_PROPAGATED,
                crate::home::KIND_DELIVERED
            ]
        );
        store
            .outbox_record(&id, Change::Ended { propagated: true }, 500, u64::MAX)
            .expect("record");
        assert!(store.outbox_open().expect("open").is_empty());
    }

    fn mark(store: &mut Store, device: [u8; 16], mark: &wire::Mark) -> Status {
        let answer = answer(
            store,
            &home(),
            crate::home::MARK_PATH,
            &wire::mark_request(mark),
            Some(device),
            300.0,
        );
        read_status(&answer.bytes).expect("status")
    }

    #[test]
    fn a_mark_is_one_archive_entry_however_often_it_comes() {
        let (_dir, mut store) = store();
        let read = wire::Mark {
            flag: wire::MarkFlag::Read,
            ids: alloc::vec![[7; 32]],
        };
        // Not admitted: refused, nothing kept.
        assert_eq!(mark(&mut store, LAPTOP, &read), Status::NotAdmitted);
        admitted(&mut store);
        assert_eq!(mark(&mut store, LAPTOP, &read), Status::Ok);
        assert_eq!(mark(&mut store, LAPTOP, &read), Status::Ok);
        let deleted = wire::Mark {
            flag: wire::MarkFlag::Deleted,
            ids: alloc::vec![[7; 32]],
        };
        assert_eq!(mark(&mut store, LAPTOP, &deleted), Status::Ok);
        let page = store.archive_since(0, usize::MAX).expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [crate::home::KIND_MARK, crate::home::KIND_MARK]
        );
        assert_eq!(wire::read_mark(&page.entries[0].packed), Ok(read));
        assert_eq!(wire::read_mark(&page.entries[1].packed), Ok(deleted));
        // A mark that does not decode is refused.
        let answer = answer(
            &mut store,
            &home(),
            crate::home::MARK_PATH,
            &[0x90],
            Some(LAPTOP),
            1.0,
        );
        assert_eq!(read_status(&answer.bytes), Ok(Status::Malformed));
    }

    fn petname(store: &mut Store, device: [u8; 16], peer: [u8; 16], name: Option<&str>) -> Status {
        let answer = answer(
            store,
            &home(),
            crate::home::PETNAME_PATH,
            &wire::petname_entry(&peer, name),
            Some(device),
            300.0,
        );
        read_status(&answer.bytes).expect("status")
    }

    /// A contact's key (1187) is taken from an admitted device when its
    /// address is the one the key makes, archives nothing, and comes back
    /// for the daemon to remember; a mismatched pair or a stranger's
    /// request is refused with nothing learned.
    #[test]
    fn a_contact_key_is_learned_only_when_it_matches_and_the_device_is_admitted() {
        let (_dir, mut store) = store();
        let identity = Identity::generate(&mut rand_core::OsRng);
        let key = identity.public_key_bytes();
        let peer = crate::home::lxmf_address(&key).expect("address");
        let ask = |store: &mut Store, peer: [u8; 16]| {
            let answer = answer(
                store,
                &home(),
                crate::home::KEY_PATH,
                &wire::key_request(&peer, &key),
                Some(LAPTOP),
                300.0,
            );
            (read_status(&answer.bytes).expect("status"), answer.learned)
        };
        assert_eq!(ask(&mut store, peer), (Status::NotAdmitted, None));
        admitted(&mut store);
        assert_eq!(ask(&mut store, peer), (Status::Ok, Some((peer, key))));
        let mut wrong = peer;
        wrong[0] ^= 1;
        assert_eq!(ask(&mut store, wrong), (Status::Malformed, None));
        assert!(store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .is_empty());
    }

    /// A petname is an archive entry for an admitted device, only the
    /// newest of a peer stays, the same one again appends nothing, and it
    /// wakes nobody (1139).
    #[test]
    fn a_petname_is_the_newest_entry_of_its_peer_and_wakes_nobody() {
        let (_dir, mut store) = store();
        let (alice, bob) = ([0xa1; 16], [0xb0; 16]);
        assert_eq!(
            petname(&mut store, LAPTOP, alice, Some("Mum")),
            Status::NotAdmitted
        );
        assert!(store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .is_empty());
        admitted(&mut store);
        let woken = store.archive_newest_waking().expect("waking");
        assert_eq!(petname(&mut store, LAPTOP, alice, Some("Mum")), Status::Ok);
        assert_eq!(petname(&mut store, LAPTOP, alice, Some("Mum")), Status::Ok);
        assert_eq!(petname(&mut store, LAPTOP, bob, Some("Bob")), Status::Ok);
        assert_eq!(petname(&mut store, LAPTOP, alice, None), Status::Ok);
        let page = store.archive_since(0, usize::MAX).expect("page");
        let kept: Vec<_> = page
            .entries
            .iter()
            .inspect(|e| assert_eq!(e.kind, crate::home::KIND_PETNAME))
            .map(|e| wire::read_petname_entry(&e.packed).expect("a petname"))
            .collect();
        assert_eq!(kept, [(bob, Some("Bob".into())), (alice, None)]);
        assert_eq!(store.archive_newest_waking().expect("waking"), woken);
        let answer = answer(
            &mut store,
            &home(),
            crate::home::PETNAME_PATH,
            &[0x90],
            Some(LAPTOP),
            1.0,
        );
        assert_eq!(read_status(&answer.bytes), Ok(Status::Malformed));
    }

    /// A peer state is an archive entry for an admitted device, only the
    /// newest of a peer stays beside its petname, and it wakes nobody
    /// (1146).
    #[test]
    fn a_peer_state_is_the_newest_entry_of_its_peer_and_wakes_nobody() {
        let (_dir, mut store) = store();
        let alice = [0xa1; 16];
        let ask = |store: &mut Store, state: &wire::PeerState| {
            let answer = answer(
                store,
                &home(),
                crate::home::PEERSTATE_PATH,
                &wire::peer_state_entry(&alice, state),
                Some(LAPTOP),
                300.0,
            );
            read_status(&answer.bytes).expect("status")
        };
        let blocked = wire::PeerState {
            blocked: true,
            ..wire::PeerState::default()
        };
        assert_eq!(ask(&mut store, &blocked), Status::NotAdmitted);
        admitted(&mut store);
        let woken = store.archive_newest_waking().expect("waking");
        assert_eq!(petname(&mut store, LAPTOP, alice, Some("Mum")), Status::Ok);
        assert_eq!(ask(&mut store, &blocked), Status::Ok);
        assert_eq!(ask(&mut store, &blocked), Status::Ok);
        let muted = wire::PeerState {
            muted_until: Some(i64::MAX),
            ..wire::PeerState::default()
        };
        assert_eq!(ask(&mut store, &muted), Status::Ok);
        let page = store.archive_since(0, usize::MAX).expect("page");
        let kinds: Vec<_> = page.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [crate::home::KIND_PETNAME, crate::home::KIND_PEERSTATE]
        );
        assert_eq!(
            wire::read_peer_state_entry(&page.entries[1].packed),
            Ok((alice, muted))
        );
        assert_eq!(store.archive_newest_waking().expect("waking"), woken);
    }

    /// `/watch` is answered with the newest sequence and keeps the link,
    /// for an admitted device only.
    #[test]
    fn a_watch_names_the_newest_sequence_for_an_admitted_device_only() {
        let (_dir, mut store) = store();
        let ask = |store: &mut Store, device| {
            answer(
                store,
                &home(),
                crate::home::WATCH_PATH,
                &wire::watch_request(0),
                device,
                1.0,
            )
        };
        let refused = ask(&mut store, Some(LAPTOP));
        assert_eq!(read_status(&refused.bytes), Ok(Status::NotAdmitted));
        assert_eq!(refused.watching, None);
        assert_eq!(
            read_status(&ask(&mut store, None).bytes),
            Ok(Status::NotIdentified)
        );
        admitted(&mut store);
        let empty = ask(&mut store, Some(LAPTOP));
        assert_eq!(wire::read_watch_answer(&empty.bytes), Ok(Ok(0)));
        assert_eq!(empty.watching, Some(0));
        archive(&mut store, 1);
        let newest = archive(&mut store, 2);
        let full = ask(&mut store, Some(LAPTOP));
        assert_eq!(wire::read_watch_answer(&full.bytes), Ok(Ok(newest)));
        assert_eq!(full.watching, Some(newest));
        assert_eq!(store.archive_newest().expect("newest"), newest);
    }

    #[test]
    fn an_admission_token_expires() {
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 0).expect("issue");
        store.issue_token(&[0x44; 16], "desk", 0).expect("issue");
        store.issue_token(&[0x45; 16], "late", DAY).expect("issue");
        let open = store.open_tokens(DAY - 1, DAY).expect("tokens");
        assert_eq!(
            open.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["desk", "laptop", "late"]
        );
        // A second before the day is up it still admits.
        assert_eq!(
            join_at(&mut store, LAPTOP, TOKEN, (DAY - 1) as f64),
            Status::Ok
        );
        // At the day it is refused, and gone.
        assert_eq!(
            join_at(&mut store, [0x55; 16], [0x44; 16], DAY as f64),
            Status::BadToken
        );
        let open = store.open_tokens(DAY, DAY).expect("tokens");
        assert_eq!(
            open.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["late"]
        );
        assert_eq!(open[0].token, [0x45; 16]);
        assert_eq!(store.open_tokens(2 * DAY, DAY).expect("tokens"), []);
    }

    #[test]
    fn a_withdrawn_token_admits_nobody() {
        use crate::store::TokenRevoked;
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 0).expect("issue");
        let mut twin = [0x44; 16];
        twin[3] = 0x01;
        store.issue_token(&[0x44; 16], "desk", 0).expect("issue");
        store.issue_token(&twin, "desk2", 0).expect("issue");
        assert_eq!(
            store.revoke_token("44").expect("revoke"),
            TokenRevoked::TooShort
        );
        assert_eq!(
            store.revoke_token("44zz").expect("revoke"),
            TokenRevoked::TooShort
        );
        assert_eq!(
            store.revoke_token("abcd").expect("revoke"),
            TokenRevoked::None
        );
        // "444444" starts both desk tokens: neither goes.
        match store.revoke_token("444444").expect("revoke") {
            TokenRevoked::Ambiguous(matched) => assert_eq!(matched.len(), 2),
            other => panic!("{other:?}"),
        }
        match store.revoke_token(" 44444401 ").expect("revoke") {
            TokenRevoked::One(gone) => assert_eq!(gone.name, "desk2"),
            other => panic!("{other:?}"),
        }
        let laptop = crate::address::to_hex(&TOKEN[..2]).to_ascii_uppercase();
        match store.revoke_token(&laptop).expect("revoke") {
            TokenRevoked::One(gone) => assert_eq!(gone.token, TOKEN),
            other => panic!("{other:?}"),
        }
        // The withdrawn code is refused as an unknown one; the other admits.
        assert_eq!(join_at(&mut store, LAPTOP, TOKEN, 1.0), Status::BadToken);
        assert_eq!(join_at(&mut store, LAPTOP, twin, 1.0), Status::BadToken);
        assert_eq!(join_at(&mut store, LAPTOP, [0x44; 16], 1.0), Status::Ok);
    }

    fn archive(store: &mut Store, n: u8) -> u64 {
        store
            .archive_append(KIND_RECEIVED, &[n; 32], &[n; 120], 50, u64::MAX)
            .expect("append")
            .expect("new")
    }

    #[test]
    fn a_good_token_admits_once_and_a_reused_one_is_refused() {
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        assert_eq!(
            store.devices().expect("devices"),
            [crate::store::Device {
                device: LAPTOP,
                name: "laptop".into(),
                admitted_at: 100,
            }]
        );
        // The same token again, from another device: used up.
        assert_eq!(join(&mut store, [0x44; 16], TOKEN), Status::BadToken);
        assert_eq!(store.devices().expect("devices").len(), 1);
        // A token never issued.
        assert_eq!(join(&mut store, [0x44; 16], [0x55; 16]), Status::BadToken);
    }

    #[test]
    fn an_unidentified_link_gets_nothing() {
        let (_dir, mut store) = store();
        archive(&mut store, 1);
        assert_eq!(sync(&mut store, None, 0), Err(Status::NotIdentified));
        let answer = answer(
            &mut store,
            &home(),
            JOIN_PATH,
            &wire::join_request(&TOKEN, "x"),
            None,
            1.0,
        );
        assert_eq!(read_status(&answer.bytes), Ok(Status::NotIdentified));
    }

    #[test]
    fn a_device_never_admitted_or_revoked_gets_nothing() {
        let (_dir, mut store) = store();
        archive(&mut store, 1);
        assert_eq!(sync(&mut store, Some(LAPTOP), 0), Err(Status::NotAdmitted));
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        assert_eq!(
            sync(&mut store, Some(LAPTOP), 0)
                .expect("page")
                .entries
                .len(),
            1
        );
        assert!(matches!(
            store.revoke("laptop").expect("revoke"),
            crate::store::Revoked::One(_)
        ));
        assert_eq!(sync(&mut store, Some(LAPTOP), 0), Err(Status::NotAdmitted));
    }

    #[test]
    fn the_cursor_gives_what_came_after_it_and_fetching_deletes_nothing() {
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        let a = archive(&mut store, 1);
        let b = archive(&mut store, 2);
        assert!(b > a);
        let all = sync(&mut store, Some(LAPTOP), 0).expect("page");
        assert_eq!(
            all.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [a, b]
        );
        assert_eq!(all.first, a);
        assert!(!all.more);
        // The same cursor twice is the same answer: idempotent, nothing gone.
        assert_eq!(sync(&mut store, Some(LAPTOP), 0).expect("page"), all);
        let after_a = sync(&mut store, Some(LAPTOP), a).expect("page");
        assert_eq!(
            after_a.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [b]
        );
        assert!(sync(&mut store, Some(LAPTOP), b)
            .expect("page")
            .entries
            .is_empty());
    }

    #[test]
    fn a_message_archived_twice_is_one_entry() {
        let (_dir, mut store) = store();
        archive(&mut store, 1);
        assert_eq!(
            store
                .archive_append(KIND_RECEIVED, &[1; 32], &[1; 120], 60, u64::MAX)
                .expect("append"),
            None
        );
        assert_eq!(
            store
                .archive_since(0, usize::MAX)
                .expect("page")
                .entries
                .len(),
            1
        );
    }

    #[test]
    fn the_cap_evicts_the_oldest_first_and_keeps_the_newest() {
        let (_dir, mut store) = store();
        let cap = 250; // two 120-byte entries fit, three do not
        let seqs: Vec<u64> = (1..=4)
            .map(|n| {
                store
                    .archive_append(KIND_RECEIVED, &[n; 32], &[n; 120], 50, cap)
                    .expect("append")
                    .expect("new")
            })
            .collect();
        let page = store.archive_since(0, usize::MAX).expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            seqs[2..]
        );
        assert_eq!(page.first, seqs[2]);
        assert_eq!(store.archive_bytes().expect("bytes"), 240);
        // One entry larger than the cap is kept until the next one comes.
        let big = store
            .archive_append(KIND_RECEIVED, &[9; 32], &[9; 400], 50, cap)
            .expect("append")
            .expect("new");
        let page = store.archive_since(0, usize::MAX).expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [big]
        );
        // And the sequence goes on growing past every eviction.
        let next = archive(&mut store, 10);
        assert!(next > big);
    }

    #[test]
    fn a_page_stops_at_its_byte_budget_but_carries_one_entry_at_least() {
        let (_dir, mut store) = store();
        for n in 1..=3 {
            archive(&mut store, n);
        }
        let page = store.archive_since(0, 200).expect("page");
        assert_eq!(page.entries.len(), 1);
        assert!(page.more);
        let page = store.archive_since(0, 240).expect("page");
        assert_eq!(page.entries.len(), 2);
        assert!(page.more);
    }

    #[test]
    fn revoke_takes_a_name_or_a_hash_and_refuses_an_ambiguous_one() {
        let (_dir, mut store) = store();
        store.issue_token(&[1; 16], "phone", 1).expect("issue");
        store.issue_token(&[2; 16], "phone", 1).expect("issue");
        store
            .redeem_token(&[1; 16], &[0xab; 16], 2, DAY)
            .expect("redeem");
        store
            .redeem_token(&[2; 16], &[0xcd; 16], 3, DAY)
            .expect("redeem");
        assert!(matches!(
            store.revoke("phone").expect("revoke"),
            crate::store::Revoked::Ambiguous(ref both) if both.len() == 2
        ));
        assert_eq!(store.devices().expect("devices").len(), 2);
        assert!(matches!(
            store.revoke("CDCD").expect("revoke"),
            crate::store::Revoked::One(ref d) if d.device == [0xcd; 16]
        ));
        assert_eq!(
            store.revoke("nobody").expect("revoke"),
            crate::store::Revoked::None
        );
        assert_eq!(store.devices().expect("devices").len(), 1);
    }

    /// A move (1281) is answered only to an admitted device with the open
    /// move's token: the identity and the archive with each id; `/moved`
    /// says Behind while the archive holds entries after the cursor, and
    /// ends the mailbox once it does not.
    #[test]
    fn a_move_goes_only_to_the_admitted_device_with_the_open_moves_token() {
        let (_dir, mut store) = store();
        admitted(&mut store);
        archive(&mut store, 1);
        archive(&mut store, 2);
        let mut serving = home();
        let ask = |store: &mut Store, home: &Home<'_>, path, device, token| {
            answer(
                store,
                home,
                path,
                &wire::move_request(&token, 0),
                Some(device),
                0.0,
            )
        };
        // No move open: the token is no key.
        let none = ask(
            &mut store,
            &serving,
            crate::home::MOVE_PATH,
            LAPTOP,
            [8; 16],
        );
        assert_eq!(read_status(&none.bytes), Ok(Status::BadToken));
        serving.handover = Some(Handover {
            token: [8; 16],
            ratchets: vec![1, 2, 3],
            known: vec![4],
            name: "Anna".into(),
        });
        let stranger = ask(
            &mut store,
            &serving,
            crate::home::MOVE_PATH,
            [2; 16],
            [8; 16],
        );
        assert_eq!(read_status(&stranger.bytes), Ok(Status::NotAdmitted));
        let other = ask(
            &mut store,
            &serving,
            crate::home::MOVE_PATH,
            LAPTOP,
            [9; 16],
        );
        assert_eq!(read_status(&other.bytes), Ok(Status::BadToken));
        let page = ask(
            &mut store,
            &serving,
            crate::home::MOVE_PATH,
            LAPTOP,
            [8; 16],
        );
        let page = wire::read_move_answer(&page.bytes)
            .expect("decodes")
            .expect("a page");
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.ratchets, [1, 2, 3]);
        assert_eq!(page.known, [4]);
        assert_eq!(page.name, "Anna");
        assert_eq!(
            page.private_key,
            serving.identity.private_key_bytes().expect("key").to_vec()
        );
        let newest = page.entries.last().map_or(0, |entry| entry.seq);
        let behind = answer(
            &mut store,
            &serving,
            crate::home::MOVED_PATH,
            &wire::move_request(&[8; 16], newest - 1),
            Some(LAPTOP),
            0.0,
        );
        assert_eq!(read_status(&behind.bytes), Ok(Status::Behind));
        assert!(!behind.gone);
        let moved = answer(
            &mut store,
            &serving,
            crate::home::MOVED_PATH,
            &wire::move_request(&[8; 16], newest),
            Some(LAPTOP),
            0.0,
        );
        assert_eq!(read_status(&moved.bytes), Ok(Status::Ok));
        assert!(moved.gone);

        // The entries land in another archive as they were, ids and all:
        // the same message again is no second entry.
        let other_dir = tempfile::tempdir().expect("dir");
        let mut new = Store::open_in(other_dir.path(), HOME).expect("store");
        assert_eq!(
            new.archive_moved_in(&page.entries, u64::MAX).expect("in"),
            2
        );
        assert_eq!(
            new.archive_moved_in(&page.entries, u64::MAX)
                .expect("again"),
            0
        );
        let (again, more, _) = new.archive_moving(0, usize::MAX).expect("read");
        assert!(!more);
        let strip = |entries: &[wire::Moving]| -> Vec<(u8, [u8; 32], i64, Vec<u8>)> {
            entries
                .iter()
                .map(|e| (e.kind, e.id, e.at, e.packed.clone()))
                .collect()
        };
        assert_eq!(strip(&again), strip(&page.entries));
    }
}
