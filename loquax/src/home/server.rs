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
    /// Where the home keeps its groups' keys (the loquax home, 1307), and
    /// the name the owner goes by in them; none where the home hosts no
    /// groups, and `/group` is refused.
    pub groups: Option<(&'a std::path::Path, &'a str)>,
    /// What `/about` says besides the devices (1642): the announced name
    /// and the machine; the version is the build's.
    pub shown: Shown,
}

/// The name and the machine `/about` says (1642).
#[derive(Debug, Clone, Default)]
pub struct Shown {
    /// The name the identity is announced under; empty for none.
    pub name: alloc::string::String,
    /// The machine's name ([`super::about::host_word`]); empty for none.
    pub host: alloc::string::String,
    /// When the daemon came up, unix seconds; 0 for unknown (1647).
    pub started: i64,
}

impl Shown {
    /// This machine's: `name` as announced, the host as the kernel says.
    pub fn here(name: &str) -> Self {
        let raw = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .or_else(|_| std::fs::read_to_string("/etc/hostname"))
            .unwrap_or_default();
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Self {
            name: alloc::string::String::from(name),
            host: super::about::host_word(&raw),
            started,
        }
    }
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
    /// A `/group` made a group (1307): the daemon starts its engine and
    /// hands it the membership for the admin.
    pub group: Option<NewGroup>,
    /// A `/groupadd` added members (1307 G2): the group, and the new
    /// membership its own engine sends the members that understand groups.
    pub added: Option<([u8; 16], Vec<super::group::Outgoing>)>,
    /// A `/groupdelete` deleted a group (1307 pass four): the group, and
    /// the last messages its engine sends before it refuses everything.
    pub deleted: Option<([u8; 16], Vec<super::group::Outgoing>)>,
}

/// A group a `/group` made: hosted from now on, and what its own engine
/// sends first.
pub struct NewGroup {
    pub hosted: super::group::Hosted,
    pub welcome: Vec<super::group::Outgoing>,
}

impl Answer {
    fn only(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            accepted: Vec::new(),
            watching: None,
            learned: None,
            gone: false,
            group: None,
            added: None,
            deleted: None,
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
    let answered = route(store, home, path, data, device, now, now_secs);
    // Every request counts as the device being there (1642), whatever it
    // asked; one that is not admitted changes nothing.
    if let Err(error) = store.device_seen(&device, now_secs) {
        tracing::warn!("HOME device seen: {error}");
    }
    answered
}

fn route(
    store: &mut Store,
    home: &Home<'_>,
    path: &str,
    data: &[u8],
    device: [u8; 16],
    now: f64,
    now_secs: i64,
) -> Answer {
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
        super::GROUP_PATH => group(store, home, data, device, now),
        super::GROUP_ADD_PATH => group_add(store, home, data, device, now),
        super::GROUP_DELETE_PATH => group_delete(store, home, data, device, now),
        super::PAGES_PATH => Answer::only(pages(store, data, device)),
        super::BOOKMARK_PATH => Answer::only(bookmark(store, data, device)),
        super::ABOUT_PATH => Answer::only(about(store, home, data, device)),
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
                group: None,
                added: None,
                deleted: None,
            }
        }
        Ok(Accepted::Again { id }) => Answer::only(wire::send_answer(&id)),
        Err(error) => {
            tracing::warn!("HOME send: {error}");
            Answer::only(wire::status_answer(Status::Failed))
        }
    }
}

/// The client id of the invitation a `/group` with `client_id` sends
/// `member`: the same request asked again finds the invitations it made
/// then in the outbox, and through them its group.
fn invitation_client_id(client_id: &[u8; 16], member: &[u8; 16]) -> [u8; 16] {
    let hash = leviculum_core::crypto::sha256(&[&b"loquax group "[..], client_id, member].concat());
    let mut id = [0u8; 16];
    id.copy_from_slice(&hash[..16]);
    id
}

/// The group a `/group` made before, by its first invitation in the outbox.
fn group_made_before(
    store: &Store,
    request: &wire::GroupRequest,
    admin: &[u8; 16],
) -> Result<Option<[u8; 16]>, crate::store::StoreError> {
    let Some((first, _)) = request.members.iter().find(|(m, _)| m != admin) else {
        return Ok(None);
    };
    let Some(id) = store.outbox_by_client(&invitation_client_id(&request.client_id, first))? else {
        return Ok(None);
    };
    Ok(store
        .archive_entry(crate::home::KIND_SENT, &id)?
        .and_then(|packed| {
            leviculum_lxmf::Message::unpack(&packed, None, None, DeliveryMethod::Direct).ok()
        })
        .and_then(|message| leviculum_lxmf::lev::Lev::from_fields(&message.fields))
        .and_then(|lev| lev.invite)
        .map(|invite| invite.group))
}

/// `/group` (1307 pass two): the home makes the group with its owner as
/// admin ([`super::group::create`]), puts an invitation to every member in
/// its outbox, and answers the group's address. Asked again with the same
/// client id, it answers the group it made then.
fn group(store: &mut Store, home: &Home<'_>, data: &[u8], device: [u8; 16], now: f64) -> Answer {
    let Ok(request) = wire::read_group_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "group") {
        return Answer::only(refused);
    }
    let Some((dir, owner_name)) = home.groups else {
        tracing::info!("HOME group refused: this mailbox hosts no groups");
        return Answer::only(wire::status_answer(Status::Failed));
    };
    match group_made_before(store, &request, &home.address) {
        Ok(Some(address)) => return Answer::only(wire::group_answer(&address)),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!("HOME group: {error}");
            return Answer::only(wire::status_answer(Status::Failed));
        }
    }
    let members: Vec<([u8; 16], alloc::string::String)> = request
        .members
        .iter()
        .filter(|(member, _)| *member != home.address)
        .cloned()
        .collect();
    if members.is_empty() {
        return Answer::only(wire::status_answer(Status::Malformed));
    }
    let made = super::group::create(
        dir,
        store,
        &request.name,
        (home.address, owner_name),
        &members,
        now as i64,
    )
    .and_then(|hosted| {
        let (invitations, welcome) =
            super::group::opening(home.identity, &hosted, owner_name, store, now)?;
        Ok((hosted, invitations, welcome))
    });
    let (hosted, invitations, welcome) = match made {
        Ok(made) => made,
        Err(error) => {
            tracing::warn!("HOME group: {error}");
            return Answer::only(wire::status_answer(Status::Failed));
        }
    };
    let at = now as i64;
    let mut accepted = Vec::new();
    for invitation in invitations {
        let client_id = invitation_client_id(&request.client_id, &invitation.destination);
        match store.outbox_accept(
            &client_id,
            &invitation.id,
            &invitation.destination,
            &invitation.packed,
            at,
            home.cap_bytes,
        ) {
            Ok(Accepted::New { .. }) => accepted.push(Outgoing {
                id: invitation.id,
                destination: invitation.destination,
                packed: invitation.packed,
                created_at: at,
                propagated: false,
            }),
            Ok(Accepted::Again { .. }) => {}
            Err(error) => {
                tracing::warn!("HOME group invitation: {error}");
                return Answer::only(wire::status_answer(Status::Failed));
            }
        }
    }
    tracing::info!(
        "HOME group made device={} group={} members={}",
        crate::address::to_hex(&device),
        crate::address::to_hex(&hosted.group.address),
        members.len()
    );
    Answer {
        bytes: wire::group_answer(&hosted.group.address),
        accepted,
        watching: None,
        learned: None,
        gone: false,
        group: Some(NewGroup { hosted, welcome }),
        added: None,
        deleted: None,
    }
}

/// `/groupdelete` (1307 pass four): the home deletes a group it hosts and
/// its owner is admin of, for everyone ([`super::group::delete`]), and
/// hands the daemon the last messages for the group's engine. A group it
/// does not host (deleted before, or never made) answers `Ok`: gone is
/// gone, so a lost answer asked again is no error. A group whose admin is
/// someone else is refused.
fn group_delete(
    store: &mut Store,
    home: &Home<'_>,
    data: &[u8],
    device: [u8; 16],
    now: f64,
) -> Answer {
    let Ok(request) = wire::read_group_delete_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "group delete") {
        return Answer::only(refused);
    }
    let Some((dir, _)) = home.groups else {
        tracing::info!("HOME group delete refused: this mailbox hosts no groups");
        return Answer::only(wire::status_answer(Status::Failed));
    };
    let hosted = match super::group::hosted(dir, store) {
        Ok(hosted) => hosted
            .into_iter()
            .find(|h| h.group.address == request.group),
        Err(error) => {
            tracing::warn!("HOME group delete: {error}");
            return Answer::only(wire::status_answer(Status::Failed));
        }
    };
    let Some(hosted) = hosted else {
        tracing::info!(
            "HOME group delete: {} is not hosted here (deleted before)",
            crate::address::to_hex(&request.group)
        );
        return Answer::only(wire::status_answer(Status::Ok));
    };
    if hosted.group.admin != home.address {
        tracing::info!(
            "HOME group delete refused: the owner is not {}'s admin",
            hosted.group.name
        );
        return Answer::only(wire::status_answer(Status::Failed));
    }
    match super::group::delete(dir, store, &hosted, now) {
        Ok(told) => Answer {
            deleted: Some((hosted.group.address, told)),
            ..Answer::only(wire::status_answer(Status::Ok))
        },
        Err(error) => {
            tracing::warn!("HOME group delete: {error}");
            Answer::only(wire::status_answer(Status::Failed))
        }
    }
}

/// `/groupadd` (1307 G2, study 4.5): the home adds the members to a group
/// it hosts and its owner is admin of ([`super::group::add`]), puts an
/// invitation to each new one in its outbox, and hands the daemon the new
/// membership for the group's engine. A member already in it is skipped,
/// so asked again it adds nobody and answers the same.
fn group_add(
    store: &mut Store,
    home: &Home<'_>,
    data: &[u8],
    device: [u8; 16],
    now: f64,
) -> Answer {
    let Ok(request) = wire::read_group_add_request(data) else {
        return Answer::only(wire::status_answer(Status::Malformed));
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "group add") {
        return Answer::only(refused);
    }
    let Some((dir, owner_name)) = home.groups else {
        tracing::info!("HOME group add refused: this mailbox hosts no groups");
        return Answer::only(wire::status_answer(Status::Failed));
    };
    let hosted = match super::group::hosted(dir, store) {
        Ok(hosted) => hosted
            .into_iter()
            .find(|h| h.group.address == request.group && h.group.admin == home.address),
        Err(error) => {
            tracing::warn!("HOME group add: {error}");
            return Answer::only(wire::status_answer(Status::Failed));
        }
    };
    let Some(hosted) = hosted else {
        tracing::info!(
            "HOME group add refused: {} is no group of this mailbox's",
            crate::address::to_hex(&request.group)
        );
        return Answer::only(wire::status_answer(Status::Failed));
    };
    let members: Vec<([u8; 16], alloc::string::String)> = request
        .members
        .iter()
        .filter(|(member, _)| *member != home.address)
        .cloned()
        .collect();
    let (invitations, told) =
        match super::group::add(home.identity, &hosted, owner_name, store, &members, now) {
            Ok(added) => added,
            Err(error) => {
                tracing::warn!("HOME group add: {error}");
                return Answer::only(wire::status_answer(Status::Failed));
            }
        };
    let at = now as i64;
    let mut accepted = Vec::new();
    for invitation in invitations {
        let client_id = invitation_client_id(&request.client_id, &invitation.destination);
        match store.outbox_accept(
            &client_id,
            &invitation.id,
            &invitation.destination,
            &invitation.packed,
            at,
            home.cap_bytes,
        ) {
            Ok(Accepted::New { .. }) => accepted.push(Outgoing {
                id: invitation.id,
                destination: invitation.destination,
                packed: invitation.packed,
                created_at: at,
                propagated: false,
            }),
            Ok(Accepted::Again { .. }) => {}
            Err(error) => {
                tracing::warn!("HOME group invitation: {error}");
                return Answer::only(wire::status_answer(Status::Failed));
            }
        }
    }
    tracing::info!(
        "HOME group added device={} group={} invited={}",
        crate::address::to_hex(&device),
        crate::address::to_hex(&request.group),
        accepted.len()
    );
    Answer {
        bytes: wire::group_answer(&request.group),
        accepted,
        watching: None,
        learned: None,
        gone: false,
        group: None,
        added: Some((request.group, told)),
        deleted: None,
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
    match mark.flag {
        wire::MarkFlag::Seen { at } => return seen(store, home, &mark.ids, at, device, now),
        wire::MarkFlag::Arrived { at } => return arrived(store, home, &mark.ids, at, device, now),
        wire::MarkFlag::Read | wire::MarkFlag::Deleted => {}
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
        // The source's own mark (1482): a relay's copy gets no receipt.
        let marked =
            leviculum_lxmf::relay::own_lev(&message.content, &message.fields, &message.source_hash)
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
        group: None,
        added: None,
        deleted: None,
    }
}

/// A device has these messages, at its clock `at` (1462): the home node
/// holds the identity, so it signs the device receipts (`lev` `0x07`). The
/// home itself never does: a proof is its word already, and it is no
/// device. The first device wins: an id an arrived mark in the archive
/// names already is passed over, and so is one a seen mark names when its
/// sender takes read receipts, since the read receipt went and read
/// includes it. The rest is kept as one arrived mark, and each sender whose
/// message asks for it ([`crate::ui::arrival::acknowledges`]) gets a
/// receipt alone, signed here and delivered from the outbox. A message the
/// archive does not hold as received gets none.
fn arrived(
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
        let (acknowledged, seen, entry) = match (
            store.archive_arrived(id),
            store.archive_seen(id),
            store.archive_entry(super::KIND_RECEIVED, id),
        ) {
            (Ok(acknowledged), Ok(seen), Ok(entry)) => (acknowledged, seen, entry),
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                tracing::warn!("HOME mark arrived: {error}");
                return Answer::only(wire::status_answer(Status::Failed));
            }
        };
        if acknowledged || new.contains(id) {
            continue;
        }
        let Some(message) = entry.and_then(|packed| {
            leviculum_lxmf::Message::unpack(&packed, None, None, DeliveryMethod::Direct).ok()
        }) else {
            continue;
        };
        // The source's own mark (1482): a relay's copy gets no receipt.
        let lev =
            leviculum_lxmf::relay::own_lev(&message.content, &message.fields, &message.source_hash);
        let read = seen && lev.as_ref().is_some_and(|lev| lev.supports_receipts());
        if read
            || message.source_hash == home.address
            || !crate::ui::arrival::acknowledges(&message.content, &message.fields, lev.as_ref())
        {
            continue;
        }
        new.push(*id);
        owed.entry(message.source_hash).or_default().push(*id);
    }
    if new.is_empty() {
        tracing::info!(
            "HOME mark arrived device={} messages={} new=0",
            crate::address::to_hex(&device),
            ids.len()
        );
        return Answer::only(wire::status_answer(Status::Ok));
    }
    let kept = wire::Mark {
        flag: wire::MarkFlag::Arrived { at },
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
    Answer {
        bytes: status,
        accepted: device_receipts(store, home, owed, at, now),
        watching: None,
        learned: None,
        gone: false,
        group: None,
        added: None,
        deleted: None,
    }
}

/// The ids of ours a received message's receipt names, read or
/// propagation (1409): the propagation node need not be asked about them
/// any more ([`crate::pn_status::Asker::settled`]).
pub fn settles(packed: &[u8]) -> Vec<[u8; 32]> {
    let Ok(message) = leviculum_lxmf::Message::unpack(packed, None, None, DeliveryMethod::Direct)
    else {
        return Vec::new();
    };
    let Some(lev) = leviculum_lxmf::lev::Lev::from_fields(&message.fields) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    if let Some(receipt) = lev.receipt {
        ids.extend(receipt.ids);
    }
    if let Some(receipt) = lev.propagation_receipt {
        ids.extend(receipt.ids);
    }
    ids
}

/// Keep what the propagation node answered as archive entries every
/// device folds (1409, [`super::KIND_PN_STATUS`]): only the words that
/// move a mark (`collected`, `delivered`, `expired`, `evicted`), at most
/// [`wire::MARK_IDS_MAX`] an entry. The newest sequence appended, none when
/// nothing moved.
pub fn keep_pn_status(
    store: &mut Store,
    cap_bytes: u64,
    answers: &[([u8; 32], leviculum_lxmf::lev_status::MessageStatus)],
    now: i64,
) -> Option<u64> {
    let moved: Vec<_> = answers
        .iter()
        .copied()
        .filter(|(_, status)| crate::pn_status::final_word(*status))
        .collect();
    let mut newest = None;
    for chunk in moved.chunks(wire::MARK_IDS_MAX) {
        let data = wire::pn_status_entry(chunk);
        let id = leviculum_core::crypto::sha256(&data);
        match store.archive_append(super::KIND_PN_STATUS, &id, &data, now, cap_bytes) {
            Ok(seq) => {
                tracing::info!(
                    "HOME pn status ids={} seq={}",
                    chunk.len(),
                    seq.map_or_else(|| "-".to_string(), |seq| seq.to_string())
                );
                newest = newest.max(seq);
            }
            Err(error) => tracing::warn!("HOME pn status: {error}"),
        }
    }
    newest
}

/// Sign the device receipts `owed` (1462), per sender the ids a device
/// said it has, at the device's clock `received_at`, and put each in the
/// outbox as the seen mark puts a read receipt: the new outbox entries,
/// delivered direct first and then by the propagation node.
fn device_receipts(
    store: &mut Store,
    home: &Home<'_>,
    owed: alloc::collections::BTreeMap<[u8; 16], Vec<[u8; 32]>>,
    received_at: u32,
    now: f64,
) -> Vec<Outgoing> {
    let mut accepted = Vec::new();
    for (peer, ids) in owed {
        for receipt in leviculum_lxmf::lev::PropagationReceipt::batches(received_at, &ids) {
            let count = receipt.ids.len();
            match lev_out(store, home, peer, crate::ui::arrival::alone(receipt), now) {
                Ok(Some(outgoing)) => {
                    tracing::info!(
                        "HOME device receipt to={} ids={count} received_at={received_at} message={}",
                        crate::address::to_hex(&peer),
                        crate::address::to_hex(&outgoing.id)
                    );
                    accepted.push(outgoing);
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    "HOME device receipt to {}: {error}",
                    crate::address::to_hex(&peer)
                ),
            }
        }
    }
    accepted
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
    lev_out(
        store,
        home,
        peer,
        leviculum_lxmf::lev::Lev::mark_with(receipt),
        now,
    )
}

/// Sign a message to `peer` that is nothing but the namespace `lev`, and
/// put it in the outbox under its own id.
fn lev_out(
    store: &mut Store,
    home: &Home<'_>,
    peer: [u8; 16],
    lev: leviculum_lxmf::lev::Lev,
    now: f64,
) -> Result<Option<Outgoing>, alloc::string::String> {
    // The map names its writer (1482), as a device's own does.
    let lev = leviculum_lxmf::lev::Lev {
        writer: Some(home.address),
        ..lev
    };
    let message = leviculum_lxmf::Message::create(
        peer,
        home.address,
        home.identity,
        now,
        Vec::new(),
        Vec::new(),
        lev.fields().to_vec(),
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
/// `/pages` (1579): the page and bookmark rows after the device's stamp,
/// cut to its bound or to [`super::pages::PAGES_ANSWER_MAX`]. A stamp past
/// the home's newest is a device that counted with another home (a
/// move, a store made again): it gets the whole list.
fn pages(store: &mut Store, data: &[u8], device: [u8; 16]) -> Vec<u8> {
    use super::pages;
    let Ok((since, bound)) = pages::read_pages_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "pages") {
        return refused;
    }
    let bytes = bound
        .and_then(|b| usize::try_from(b).ok())
        .unwrap_or(pages::PAGES_ANSWER_MAX)
        .min(pages::PAGES_ANSWER_MAX);
    let read = store.pages_newest().and_then(|newest| {
        let since = if since > newest { 0 } else { since };
        // A row takes at least 20 bytes, so no answer holds more.
        let limit = (bytes / 20).max(1);
        store
            .pages_since(since, limit)
            .map(|(rows, behind)| (since, rows, behind))
    });
    match read {
        Ok((since, rows, behind)) => {
            let delta = pages::fit(rows, since, bytes, behind);
            let answer = pages::pages_answer(&delta);
            tracing::info!(
                "HOME pages device={} since={since} rows={} stamp={} more={} bytes={}",
                crate::address::to_hex(&device),
                delta.rows.len(),
                delta.stamp,
                delta.more,
                answer.len()
            );
            answer
        }
        Err(error) => {
            tracing::warn!("HOME pages: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

/// `/bookmark` (1579): a device's bookmark set or cleared; kept when it is
/// newer than the home's, and `Ok` either way, since the newer word is the
/// one every device gets.
fn bookmark(store: &mut Store, data: &[u8], device: [u8; 16]) -> Vec<u8> {
    let Ok(row) = super::pages::read_bookmark_request(data) else {
        return wire::status_answer(Status::Malformed);
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "bookmark") {
        return refused;
    }
    match store.bookmark_put(&row) {
        Ok(stamp) => {
            tracing::info!(
                "HOME bookmark device={} deleted={} at={} stamp={}",
                crate::address::to_hex(&device),
                row.deleted,
                row.at,
                stamp.map_or_else(|| "-".to_string(), |s| s.to_string())
            );
            wire::status_answer(Status::Ok)
        }
        Err(error) => {
            tracing::warn!("HOME bookmark: {error}");
            wire::status_answer(Status::Failed)
        }
    }
}

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

/// What the mailbox says of itself (1642): its name and machine, the
/// build's version, and every device it admitted with when it was last
/// there. An admitted device only: the list names the owner's devices.
fn about(store: &Store, home: &Home<'_>, data: &[u8], device: [u8; 16]) -> Vec<u8> {
    if super::about::read_about_request(data).is_err() {
        return wire::status_answer(Status::Malformed);
    }
    if let Some(refused) = refuse_unless_admitted(store, &device, "about") {
        return refused;
    }
    let devices = match store.devices() {
        Ok(devices) => devices,
        Err(error) => {
            tracing::warn!("HOME about: {error}");
            return wire::status_answer(Status::Failed);
        }
    };
    tracing::info!(
        "HOME about device={} devices={}",
        crate::address::to_hex(&device),
        devices.len()
    );
    super::about::about_answer(&super::about::About {
        name: home.shown.name.clone(),
        host: home.shown.host.clone(),
        version: alloc::string::String::from(crate::VERSION),
        devices: devices
            .into_iter()
            .map(|d| super::about::KnownDevice {
                device: d.device,
                name: d.name,
                admitted_at: d.admitted_at,
                last_seen: d.last_seen,
            })
            .collect(),
        started: Some(home.shown.started).filter(|at| *at > 0),
    })
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
                group: None,
                added: None,
                deleted: None,
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
        group: None,
        added: None,
        deleted: None,
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
            // The page carries this home's ring, which the device keeps as
            // its own: both hold the same ring after it, or neither has one.
            crate::events::home_sync(&device, 0, entries.len(), "same");
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
                group: None,
                added: None,
                deleted: None,
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
    let Ok((cursor, asked, fold)) = wire::read_sync_asked(data) else {
        return wire::status_answer(Status::Malformed);
    };
    if let Some(refused) = refuse_unless_admitted(store, &device, "sync") {
        return refused;
    }
    // A client on LoRa asks a page that fits one packet (1363); the
    // archive's own bound counts the messages, the cut the whole answer.
    let bound = asked.map_or(super::SYNC_PAGE_BYTES, |asked| {
        usize::try_from(asked).map_or(super::SYNC_PAGE_BYTES, |asked| {
            asked.min(super::SYNC_PAGE_BYTES)
        })
    });
    // A device that says what it folds (1439) is served only that, and
    // `left` counts only that.
    match store.archive_since_folded(cursor, bound, fold) {
        Ok(mut page) => {
            if asked.is_some() {
                // A client that asks a bound reads how many wait (1415);
                // the cut counts with the longest such number, so the
                // real one never pushes the answer past the bound.
                page.left = Some(wire::LEFT_UNCOUNTED);
                wire::fit_page(&mut page, bound);
                let after = page.entries.last().map_or(cursor, |entry| entry.seq);
                match store.archive_count_after_folded(after, fold) {
                    Ok(left) => page.left = Some(left),
                    Err(error) => {
                        tracing::warn!("HOME sync: {error}");
                        return wire::status_answer(Status::Failed);
                    }
                }
            }
            tracing::info!(
                "HOME sync device={} cursor={cursor} entries={} more={} left={} fold={}",
                crate::address::to_hex(&device),
                page.entries.len(),
                page.more,
                page.left.map_or(-1, |left| left as i64),
                fold.word().unwrap_or("all")
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
            groups: None,
            shown: Shown {
                name: "Lew Palm".into(),
                host: "miauhaus".into(),
                started: 90,
            },
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

    fn pages_ask(
        store: &mut Store,
        device: [u8; 16],
        since: u64,
        bound: Option<u64>,
    ) -> Result<crate::home::pages::Delta, Status> {
        let answer = answer(
            store,
            &home(),
            crate::home::PAGES_PATH,
            &crate::home::pages::pages_request(since, bound),
            Some(device),
            100.0,
        );
        crate::home::pages::read_pages_answer(&answer.bytes).expect("answer")
    }

    /// 1579: `/pages` refuses a device not admitted; an admitted one gets
    /// the whole list from stamp 0 page by page at its bound, then the
    /// delta after its stamp, and a bookmark one device set reaches the
    /// next ask; a stamp past the home's newest gets the list again.
    #[test]
    fn pages_bring_the_list_then_the_delta_and_a_bookmark_comes_back() {
        use crate::home::pages::{self, BookmarkRow};
        let (_dir, mut store) = store();
        assert_eq!(
            pages_ask(&mut store, LAPTOP, 0, None),
            Err(Status::NotAdmitted)
        );
        admitted(&mut store);
        for i in 0..2000u32 {
            let mut dest = [0u8; 16];
            dest[..4].copy_from_slice(&i.to_be_bytes());
            store
                .page_heard(&lnomad::discovery::DiscoveredNode {
                    dest_hash: dest,
                    name: Some(alloc::format!("NomadNet node {i}")),
                    first_seen: 1_800_000_000,
                    last_seen: 1_800_000_000,
                    hops: Some(3),
                })
                .expect("heard");
        }
        let mut stamp = 0;
        let mut rows = 0;
        let mut asks = 0;
        let mut bytes = 0;
        loop {
            let answer = answer(
                &mut store,
                &home(),
                crate::home::PAGES_PATH,
                &pages::pages_request(stamp, None),
                Some(LAPTOP),
                100.0,
            );
            bytes += answer.bytes.len();
            assert!(answer.bytes.len() <= pages::PAGES_ANSWER_MAX);
            let delta = pages::read_pages_answer(&answer.bytes)
                .expect("answer")
                .expect("ok");
            asks += 1;
            rows += delta.rows.len();
            stamp = delta.stamp;
            if !delta.more {
                break;
            }
        }
        assert_eq!(rows, 2000);
        assert!((3..=6).contains(&asks), "{asks} asks, {bytes} bytes");
        let lora = pages_ask(
            &mut store,
            LAPTOP,
            0,
            Some(crate::home::ask::LORA_PAGE_BYTES as u64),
        )
        .expect("ok");
        assert!(lora.more && !lora.rows.is_empty());
        assert!(pages::pages_answer(&lora).len() <= crate::home::ask::LORA_PAGE_BYTES);
        assert!(pages_ask(&mut store, LAPTOP, stamp, None)
            .expect("ok")
            .rows
            .is_empty());
        let set = BookmarkRow {
            url: "00000001000000000000000000000000:/page/index.mu".into(),
            title: "Node one".into(),
            at: 1_800_000_000_000,
            deleted: false,
        };
        let taken = answer(
            &mut store,
            &home(),
            crate::home::BOOKMARK_PATH,
            &pages::bookmark_request(&set),
            Some(LAPTOP),
            100.0,
        );
        assert_eq!(read_status(&taken.bytes), Ok(Status::Ok));
        let delta = pages_ask(&mut store, LAPTOP, stamp, None).expect("ok");
        assert_eq!(delta.bookmarks().collect::<Vec<_>>(), [&set]);
        let again = pages_ask(&mut store, LAPTOP, delta.stamp + 1000, None).expect("ok");
        assert!(again.more && !again.rows.is_empty(), "another home's stamp");
    }

    /// 1348: a German admin's phone of 1307 pass six still sends its
    /// language as a fourth element; the home reads it past, and the
    /// invitation it signs is the English text, the bytes
    /// [`super::super::group::invitation_text`] makes for no language at
    /// all. Control: the same request without the element makes the same
    /// text for its own group.
    #[test]
    fn a_german_admins_invitations_are_english_on_the_wire() {
        let invited = |legacy_de: bool| {
            let (dir, mut store) = store();
            let mut hosting = home();
            hosting.groups = Some((dir.path(), "Lew"));
            admitted(&mut store);
            let mut request = wire::group_request(&wire::GroupRequest {
                client_id: [7; 16],
                name: "Familie".into(),
                members: alloc::vec![([0x44; 16], "Anna".into())],
            });
            if legacy_de {
                request[0] = 0x94;
                leviculum_lxmf::msgpack::string(&mut request, "de");
            }
            let made = answer(
                &mut store,
                &hosting,
                crate::home::GROUP_PATH,
                &request,
                Some(LAPTOP),
                300.0,
            );
            let group = wire::read_group_answer(&made.bytes)
                .expect("answer")
                .expect("made");
            let [invitation] = made.accepted.as_slice() else {
                panic!("one invitation: {:?}", made.accepted.len());
            };
            let message = leviculum_lxmf::Message::unpack(
                &invitation.packed,
                None,
                None,
                DeliveryMethod::Direct,
            )
            .expect("unpack");
            let key = super::super::group::hosted(dir.path(), &store)
                .expect("hosted")
                .into_iter()
                .find(|h| h.group.address == group)
                .expect("ours")
                .identity
                .public_key_bytes();
            let code = crate::lxma::encode_parts(&group, &key);
            (
                alloc::string::String::from_utf8(message.content).expect("text"),
                super::super::group::invitation_text("Lew", "Familie", &code, true),
            )
        };
        let (german, english) = invited(true);
        assert_eq!(german, english);
        assert!(
            german.starts_with("Lew invited you to Familie: lxma://"),
            "{german}"
        );
        let (plain, english) = invited(false);
        assert_eq!(plain, english, "control: no fourth element");
    }

    /// 1307 G2, item 6 (study 4.5): `/groupadd` from an admitted device
    /// adds Cara to the owner's group: her invitation goes into the
    /// outbox, the daemon gets the new list for the group's engine.
    /// Controls: the same request again adds nobody; a group that is not
    /// this home's is refused; a device not admitted is refused.
    #[test]
    fn a_member_is_added_once_to_the_owners_group() {
        let (dir, mut store) = store();
        let mut hosting = home();
        hosting.groups = Some((dir.path(), "Lew"));
        admitted(&mut store);
        let made = answer(
            &mut store,
            &hosting,
            crate::home::GROUP_PATH,
            &wire::group_request(&wire::GroupRequest {
                client_id: [7; 16],
                name: "Familie".into(),
                members: alloc::vec![([0x44; 16], "Anna".into())],
            }),
            Some(LAPTOP),
            300.0,
        );
        let group = wire::read_group_answer(&made.bytes)
            .expect("answer")
            .expect("made");
        let request = wire::GroupAddRequest {
            client_id: [8; 16],
            group,
            members: alloc::vec![([0x66; 16], "Cara".into()), ([0x44; 16], "Anna".into())],
        };
        let ask = |store: &mut Store, device, request: &wire::GroupAddRequest| {
            answer(
                store,
                &hosting,
                crate::home::GROUP_ADD_PATH,
                &wire::group_add_request(request),
                Some(device),
                400.0,
            )
        };
        let added = ask(&mut store, LAPTOP, &request);
        assert_eq!(wire::read_group_answer(&added.bytes), Ok(Ok(group)));
        assert_eq!(added.accepted.len(), 1, "Cara's invitation, not Anna's");
        assert_eq!(added.accepted[0].destination, [0x66; 16]);
        let (to, told) = added.added.expect("the list for the engine");
        assert_eq!(to, group);
        assert_eq!(
            told.len(),
            1,
            "the admin, the one member that understands groups"
        );
        let members = store.group_members(&group).expect("members");
        assert!(members
            .iter()
            .any(|m| m.address == [0x66; 16] && m.present()));
        assert_eq!(store.group(&group).expect("g").expect("g").epoch, 2);

        let again = ask(&mut store, LAPTOP, &request);
        assert_eq!(wire::read_group_answer(&again.bytes), Ok(Ok(group)));
        assert!(again.accepted.is_empty());
        assert!(again.added.is_some_and(|(_, told)| told.is_empty()));
        assert_eq!(store.group(&group).expect("g").expect("g").epoch, 2);

        let stranger = wire::GroupAddRequest {
            group: [0x99; 16],
            ..request.clone()
        };
        let refused = ask(&mut store, LAPTOP, &stranger);
        assert_eq!(
            wire::read_group_answer(&refused.bytes),
            Ok(Err(Status::Failed))
        );
        let refused = ask(&mut store, [0x77; 16], &request);
        assert_eq!(
            wire::read_group_answer(&refused.bytes),
            Ok(Err(Status::NotAdmitted))
        );
    }

    /// 1307 pass four, `/groupdelete`: the owner's device deletes its group;
    /// the daemon gets the last messages for the group's engine and the
    /// home hosts it no more. Asked again it is `Ok` and sends nothing
    /// (idempotent: gone is gone). Refused: a device not admitted, and a
    /// group this home hosts whose admin is someone else (control: it is
    /// still hosted after).
    #[test]
    fn the_owner_deletes_a_group_once_and_nobody_else_can() {
        let (dir, mut store) = store();
        let mut hosting = home();
        hosting.groups = Some((dir.path(), "Lew"));
        admitted(&mut store);
        let made = answer(
            &mut store,
            &hosting,
            crate::home::GROUP_PATH,
            &wire::group_request(&wire::GroupRequest {
                client_id: [7; 16],
                name: "Familie".into(),
                members: alloc::vec![([0x44; 16], "Anna".into())],
            }),
            Some(LAPTOP),
            300.0,
        );
        let group = wire::read_group_answer(&made.bytes)
            .expect("answer")
            .expect("made");
        let foreign = super::super::group::create(
            dir.path(),
            &mut store,
            "Verein",
            ([0x55; 16], "Cara"),
            &[(HOME, "Lew".into())],
            300,
        )
        .expect("a group someone else administers")
        .group
        .address;
        let ask = |store: &mut Store, device, group| {
            answer(
                store,
                &hosting,
                crate::home::GROUP_DELETE_PATH,
                &wire::group_delete_request(&wire::GroupDeleteRequest {
                    client_id: [9; 16],
                    group,
                }),
                Some(device),
                400.0,
            )
        };
        let refused = ask(&mut store, [0x77; 16], group);
        assert_eq!(wire::read_status(&refused.bytes), Ok(Status::NotAdmitted));
        assert!(store.group(&group).expect("g").is_some());

        let deleted = ask(&mut store, LAPTOP, group);
        assert_eq!(wire::read_status(&deleted.bytes), Ok(Status::Ok));
        let (gone, told) = deleted.deleted.expect("the last messages");
        assert_eq!(gone, group);
        assert_eq!(
            told.iter().map(|o| o.destination).collect::<Vec<_>>(),
            [HOME, [0x44; 16]],
            "the admin and Anna"
        );
        assert_eq!(store.group(&group).expect("g"), None);

        let again = ask(&mut store, LAPTOP, group);
        assert_eq!(wire::read_status(&again.bytes), Ok(Status::Ok));
        assert!(again.deleted.is_none(), "nothing sent twice");

        let not_ours = ask(&mut store, LAPTOP, foreign);
        assert_eq!(wire::read_status(&not_ours.bytes), Ok(Status::Failed));
        assert!(not_ours.deleted.is_none());
        assert!(
            store.group(&foreign).expect("g").is_some(),
            "control: still hosted"
        );
    }

    /// 1307 pass two: `/group` from an admitted device makes the group with
    /// the owner as admin, puts one invitation a member in the outbox (from
    /// the owner, carrying the card's `0x04`) and hands the daemon the
    /// group with the membership for the admin. The same client id again
    /// is the same group and nothing new; a home that hosts no groups, a
    /// device not admitted and a group of only the owner are refused.
    /// Positive control for the idempotence: another client id makes a
    /// second group.
    #[test]
    fn a_group_from_a_device_is_made_once_and_invites_its_members() {
        use leviculum_lxmf::lev::Lev;
        let (dir, mut store) = store();
        let request = wire::GroupRequest {
            client_id: [7; 16],
            name: "Familie".into(),
            members: alloc::vec![
                (HOME, "Lew".into()),
                ([0x44; 16], "Anna".into()),
                ([0x55; 16], "Bob".into())
            ],
        };
        let ask = |store: &mut Store, home: &Home<'_>, device, request: &wire::GroupRequest| {
            let answer = answer(
                store,
                home,
                crate::home::GROUP_PATH,
                &wire::group_request(request),
                Some(device),
                300.0,
            );
            let group = wire::read_group_answer(&answer.bytes).expect("answer");
            (group, answer)
        };
        // Not admitted yet: refused, nothing made.
        let mut hosting = home();
        hosting.groups = Some((dir.path(), "Lew"));
        let (refused, _) = ask(&mut store, &hosting, LAPTOP, &request);
        assert_eq!(refused, Err(Status::NotAdmitted));
        admitted(&mut store);
        // A home that hosts no groups refuses.
        let (refused, _) = ask(&mut store, &home(), LAPTOP, &request);
        assert_eq!(refused, Err(Status::Failed));
        assert!(store.groups().expect("groups").is_empty());

        let (made, answer) = ask(&mut store, &hosting, LAPTOP, &request);
        let group = made.expect("made");
        let new = answer.group.expect("a group for the daemon");
        assert_eq!(new.hosted.group.address, group);
        assert_eq!(new.hosted.group.admin, HOME);
        assert!(crate::home::group::key_path(dir.path(), &group).exists());
        // The owner is the admin, not invited.
        assert_eq!(
            answer.accepted.len(),
            2,
            "one invitation each to Anna and Bob"
        );
        for invitation in &answer.accepted {
            let message = leviculum_lxmf::Message::unpack(
                &invitation.packed,
                None,
                Some(hosting.identity),
                DeliveryMethod::Direct,
            )
            .expect("unpack");
            assert_eq!(message.source_hash, HOME);
            let invite = Lev::from_fields(&message.fields)
                .and_then(|lev| lev.invite)
                .expect("the card's key");
            assert_eq!(invite.group, group);
            assert_eq!(invite.name, "Familie");
            assert_eq!(invite.admin_name, "Lew");
            assert_eq!(
                crate::home::group::invited_key(&message.fields),
                Some((group, new.hosted.identity.public_key_bytes()))
            );
        }
        assert_eq!(new.welcome.len(), 1);
        assert_eq!(
            new.welcome[0].destination, HOME,
            "the membership to the admin"
        );

        let (again, answer) = ask(&mut store, &hosting, LAPTOP, &request);
        assert_eq!(again, Ok(group));
        assert!(answer.group.is_none() && answer.accepted.is_empty());
        assert_eq!(store.groups().expect("groups").len(), 1);
        let other = wire::GroupRequest {
            client_id: [8; 16],
            ..request.clone()
        };
        let (second, _) = ask(&mut store, &hosting, LAPTOP, &other);
        assert_ne!(second.expect("a second group"), group);
        assert_eq!(store.groups().expect("groups").len(), 2);

        let alone = wire::GroupRequest {
            client_id: [9; 16],
            members: alloc::vec![(HOME, "Lew".into())],
            ..request
        };
        let (refused, _) = ask(&mut store, &hosting, LAPTOP, &alone);
        assert_eq!(refused, Err(Status::Malformed));
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

    /// 1293: a leg given up leaves the row open, so a restart tries the
    /// message again, and archives one stalled entry, the first.
    #[test]
    fn a_stalled_leg_keeps_the_message_open_and_is_archived_once() {
        let dir = tempfile::tempdir().expect("dir");
        let home = home();
        let mut store = Store::open_in(dir.path(), HOME).expect("store");
        admitted(&mut store);
        let (id, _) = send(&mut store, &home, LAPTOP, &submission(1), 200.0);
        let id = id.expect("id");
        let first = store
            .outbox_record(&id, Change::Stalled, 300, u64::MAX)
            .expect("record");
        assert!(first.is_some());
        let again = store
            .outbox_record(&id, Change::Stalled, 400, u64::MAX)
            .expect("record");
        assert_eq!(again, None);
        let open = store.outbox_open().expect("open");
        assert_eq!(open.len(), 1);
        assert!(!open[0].propagated);
        let kinds: Vec<u8> = store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, [crate::home::KIND_SENT, crate::home::KIND_STALLED]);
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

    /// 1462: the home signs a device receipt only on a device's arrived
    /// mark, never on its own archiving. The first device's mark is kept
    /// and signs one receipt per sender, alone, at the device's clock; a
    /// second device's mark of the same ids signs nothing. A message
    /// without bit 2, the home's own, an id already seen by a sender who
    /// takes read receipts, and one the archive does not hold owe none.
    #[test]
    fn the_home_signs_a_device_receipt_only_on_a_devices_arrived_mark() {
        use leviculum_lxmf::lev::{Lev, CAP_GROUPS, CAP_PROPAGATION_RECEIPTS, CAP_READ_RECEIPTS};
        const PHONE: [u8; 16] = [0x44; 16];
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 10).expect("token");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        store.issue_token(&[0x34; 16], "phone", 10).expect("token");
        assert_eq!(join(&mut store, PHONE, [0x34; 16]), Status::Ok);
        let home = home();
        let sender = Identity::generate(&mut rand_core::OsRng);
        let from = crate::home::lxmf_address(&sender.public_key_bytes()).expect("address");
        let written = |caps: u64, body: &[u8], writer: Option<[u8; 16]>| {
            leviculum_lxmf::Message::create(
                HOME,
                from,
                &sender,
                100.0,
                Vec::new(),
                body.to_vec(),
                Lev {
                    capabilities: Some(caps),
                    writer,
                    ..Lev::default()
                }
                .fields()
                .to_vec(),
                DeliveryMethod::Direct,
            )
            .expect("message")
        };
        let message = |caps: u64, body: &[u8]| written(caps, body, None);
        let asks = message(CAP_GROUPS | CAP_PROPAGATION_RECEIPTS, b"hi");
        // 1482: the sender is a relay here, and these are its copies of
        // Anna's messages, her mark passed through: with her as the
        // writer, and from before the writer key with her prefix only.
        // Neither owes the relay a receipt of either kind.
        let all = CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS;
        let anna = [0xa1; 16];
        let relayed = written(all, b"Anna:\nnot a prefix", Some(anna));
        let prefixed = alloc::format!("Anna\n<{}>\nhallo", crate::address::to_hex(&anna));
        let relayed_old = written(all, prefixed.as_bytes(), None);
        let also = message(CAP_PROPAGATION_RECEIPTS, b"again");
        let silent = message(CAP_GROUPS, b"quiet");
        let read = message(CAP_READ_RECEIPTS | CAP_PROPAGATION_RECEIPTS, b"read");
        for m in [&asks, &also, &silent, &read, &relayed, &relayed_old] {
            store
                .archive_append(KIND_RECEIVED, &m.message_id, &m.pack(), 50, u64::MAX)
                .expect("archived");
        }
        // Archiving signed nothing: the outbox is empty.
        assert!(store.outbox_open().expect("outbox").is_empty());
        // Read on a device first: its read receipt names it.
        let seen = wire::Mark {
            flag: wire::MarkFlag::Seen { at: 1_799_999_000 },
            ids: vec![read.message_id, relayed.message_id, relayed_old.message_id],
        };
        let answer_of = |store: &mut Store, mark: &wire::Mark, device: [u8; 16]| {
            answer(
                store,
                &home,
                crate::home::MARK_PATH,
                &wire::mark_request(mark),
                Some(device),
                200.0,
            )
        };
        let seen_answer = answer_of(&mut store, &seen, PHONE);
        assert_eq!(seen_answer.accepted.len(), 1);
        let read_receipt = leviculum_lxmf::Message::unpack(
            &seen_answer.accepted[0].packed,
            None,
            None,
            DeliveryMethod::Direct,
        )
        .expect("the read receipt");
        assert_eq!(
            Lev::from_fields(&read_receipt.fields).and_then(|l| l.receipt.map(|r| r.ids)),
            Some(vec![read.message_id]),
            "no read receipt for the relayed copies"
        );
        let mark = wire::Mark {
            flag: wire::MarkFlag::Arrived { at: 1_800_000_000 },
            ids: vec![
                asks.message_id,
                also.message_id,
                silent.message_id,
                read.message_id,
                relayed.message_id,
                relayed_old.message_id,
                [0x77; 32],
            ],
        };
        let first = answer_of(&mut store, &mark, LAPTOP);
        assert_eq!(read_status(&first.bytes), Ok(Status::Ok));
        assert_eq!(first.accepted.len(), 1, "one receipt for the one sender");
        assert_eq!(first.accepted[0].destination, from);
        let receipt = leviculum_lxmf::Message::unpack(
            &first.accepted[0].packed,
            None,
            None,
            DeliveryMethod::Direct,
        )
        .expect("the receipt");
        assert_eq!(receipt.source_hash, HOME, "signed by the home's identity");
        let lev = Lev::from_fields(&receipt.fields).expect("lev");
        assert_eq!(lev.capabilities, None);
        assert_eq!(lev.writer, Some(HOME), "the map names its writer");
        assert_eq!(
            lev.propagation_receipt.map(|r| (r.received_at, r.ids)),
            Some((1_800_000_000, vec![asks.message_id, also.message_id]))
        );
        assert!(receipt.content.is_empty());
        assert!(store.archive_arrived(&asks.message_id).expect("kept"));
        assert!(!store.archive_arrived(&silent.message_id).expect("not kept"));
        // The phone pulled them too: the laptop said it first.
        let second = answer_of(&mut store, &mark, PHONE);
        assert_eq!(read_status(&second.bytes), Ok(Status::Ok));
        assert!(second.accepted.is_empty());
        // A stranger's mark is refused and signs nothing.
        let stranger = answer_of(&mut store, &mark, [0x55; 16]);
        assert_eq!(read_status(&stranger.bytes), Ok(Status::NotAdmitted));
        assert!(stranger.accepted.is_empty());
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

    /// 1642: `/about` names the mailbox, its machine and version, and
    /// every device with when it was last there; each request moves that,
    /// at most once a minute; a device not admitted is refused.
    #[test]
    fn about_names_the_mailbox_and_its_devices_with_when_each_was_there() {
        use crate::home::about::{about_request, read_about_answer};
        let (_dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        let ask = |store: &mut Store, device, now| {
            let answer = answer(
                store,
                &home(),
                crate::home::ABOUT_PATH,
                &about_request(),
                Some(device),
                now,
            );
            read_about_answer(&answer.bytes).expect("decodes")
        };
        let about = ask(&mut store, LAPTOP, 130.0).expect("answered");
        assert_eq!(about.name, "Lew Palm");
        assert_eq!(about.host, "miauhaus");
        assert_eq!(about.version, crate::VERSION);
        assert_eq!(about.started, Some(90), "when the daemon came up");
        assert_eq!(about.devices.len(), 1);
        assert_eq!(about.devices[0].name, "laptop");
        // Thirty seconds after the join: the minute has not passed.
        assert_eq!(about.devices[0].last_seen, Some(100));
        let later = ask(&mut store, LAPTOP, 400.0).expect("answered");
        assert_eq!(
            later.devices[0].last_seen,
            Some(100),
            "the ask at 130 was inside the minute, the one at 400 is kept after its answer"
        );
        assert_eq!(store.devices().expect("devices")[0].last_seen, Some(400));
        assert_eq!(ask(&mut store, [0x44; 16], 500.0), Err(Status::NotAdmitted));
        assert_eq!(
            store.devices().expect("devices").len(),
            1,
            "a stranger's request is no device"
        );
    }

    /// 1647, walk 6, finding 29: after the schema 20 migration every
    /// device the mailbox already held has no `last_seen`. Its `/about`
    /// says so (0 on the wire), and the page says such a device was never
    /// connected (1667, walk 11, finding 57: a bare name read as a broken
    /// row); the asking device gets its time on its first request.
    #[test]
    fn a_migrated_mailbox_names_its_old_devices_as_never_connected() {
        use crate::home::about::{about_request, read_about_answer};
        use crate::ui::mailbox_page::{page, Told};
        use crate::ui::profile::{Mailbox, ProfileFacts};
        use crate::ui::strings::Lang;
        const PHONE: [u8; 16] = [0x44; 16];
        let (dir, mut store) = store();
        store.issue_token(&TOKEN, "laptop", 90).expect("issue");
        assert_eq!(join(&mut store, LAPTOP, TOKEN), Status::Ok);
        store
            .issue_token(&[0x34; 16], "Fairphone", 90)
            .expect("issue");
        assert_eq!(join(&mut store, PHONE, [0x34; 16]), Status::Ok);
        drop(store);
        // Back to schema 19, as Lew's miauhaus was before 1642.
        let conn =
            rusqlite::Connection::open(dir.path().join(crate::store::STORE_FILE)).expect("raw");
        conn.execute_batch(
            "ALTER TABLE home_devices DROP COLUMN last_seen; DROP TABLE owed_peer_states; \
             DROP TABLE home_names; PRAGMA user_version = 19;",
        )
        .expect("made schema 19");
        drop(conn);
        let mut store = Store::open_in(dir.path(), HOME).expect("migrated");
        let first = answer(
            &mut store,
            &home(),
            crate::home::ABOUT_PATH,
            &about_request(),
            Some(PHONE),
            1_000.0,
        );
        let about = read_about_answer(&first.bytes)
            .expect("decodes")
            .expect("answered");
        assert!(
            about.devices.iter().all(|d| d.last_seen.is_none()),
            "{about:?}"
        );
        let mut model = crate::ui::UiModel::<crate::ui::tests::Field>::new(&[]);
        model.address = Some(HOME);
        model.now = Some(crate::when::WallTime {
            unix_secs: 1_000,
            offset_secs: 0,
        });
        model.profile = ProfileFacts {
            mailbox: Mailbox::Away {
                name: None,
                rung: None,
                word: None,
            },
            told: Told::Told(about),
            device: Some(PHONE),
            version: alloc::string::String::from(crate::VERSION),
            ..ProfileFacts::default()
        };
        let shown = page(&model);
        let said: Vec<_> = shown
            .rows()
            .iter()
            .map(|row| match &row.value {
                Some(value) => alloc::format!(
                    "{}: {}",
                    row.label.resolve(Lang::De),
                    value.resolve(Lang::De)
                ),
                None => row.label.resolve(Lang::De),
            })
            .collect();
        println!(
            "MIGRATED mailboxpage:\n{}",
            serde_json::to_string_pretty(&shown.to_json()).expect("json")
        );
        // Walk 11, finding 57 (1667): never heard says so.
        assert!(
            said.iter().any(|l| l == "laptop: noch nie verbunden"),
            "{said:#?}"
        );
        assert!(
            said.iter().any(|l| l == "Fairphone: dieses Gerät"),
            "{said:#?}"
        );
        assert!(
            !said.iter().any(|l| l.contains("noch nicht da")),
            "{said:#?}"
        );
        // The asking device has its time once it asked.
        let devices = store.devices().expect("devices");
        let phone = devices.iter().find(|d| d.device == PHONE).expect("phone");
        assert_eq!(phone.last_seen, Some(1_000));
        let laptop = devices.iter().find(|d| d.device == LAPTOP).expect("laptop");
        assert_eq!(laptop.last_seen, None, "the laptop has not asked");
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
                // The join is the device's first request: there now.
                last_seen: Some(100),
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

    /// 1363: a client on LoRa asks pages its link carries as one packet.
    /// Each answer keeps the bound, carries one entry at least, and the
    /// pages walked by cursor bring every entry once. Control: the same
    /// archive asked without a bound comes as one page over the bound.
    #[test]
    fn a_page_asked_with_a_bound_keeps_it_and_the_walk_misses_nothing() {
        let (_dir, mut store) = store();
        admitted(&mut store);
        let seqs: Vec<u64> = (1..=5).map(|n| archive(&mut store, n)).collect();
        let bound = crate::home::ask::LORA_PAGE_BYTES;
        let asked = |store: &mut Store, cursor: u64| {
            let data = wire::sync_request_paged(cursor, bound as u64);
            let answer = answer(store, &home(), SYNC_PATH, &data, Some(LAPTOP), 100.0);
            (
                answer.bytes.len(),
                read_sync_answer(&answer.bytes).expect("answer"),
            )
        };
        let (mut cursor, mut walked) = (0, Vec::new());
        loop {
            let (len, page) = asked(&mut store, cursor);
            let page = page.expect("page");
            assert!(len <= bound, "{len} B over the bound {bound}");
            assert!(!page.entries.is_empty());
            walked.extend(page.entries.iter().map(|e| e.seq));
            cursor = page.entries.last().expect("one").seq;
            // 1415: the page says how many wait after it, exactly.
            assert_eq!(page.left, Some((seqs.len() - walked.len()) as u64));
            if !page.more {
                break;
            }
        }
        assert_eq!(walked, seqs);
        let whole = answer(
            &mut store,
            &home(),
            SYNC_PATH,
            &sync_request(0),
            Some(LAPTOP),
            100.0,
        );
        assert!(whole.bytes.len() > bound);
        let whole = read_sync_answer(&whole.bytes)
            .expect("answer")
            .expect("page");
        assert_eq!(whole.entries.len(), 5);
        // A client that asked no bound gets the four elements it reads.
        assert_eq!(whole.left, None);
        // One entry larger than the bound still comes, alone.
        let big = store
            .archive_append(KIND_RECEIVED, &[9; 32], &[9; 600], 50, u64::MAX)
            .expect("append")
            .expect("new");
        let (len, page) = asked(&mut store, cursor);
        let page = page.expect("page");
        assert_eq!(
            page.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [big]
        );
        assert!(len > bound && !page.more);
    }

    /// 1439: a home whose archive opens with 800 imported entries, then
    /// 100 a board shows, a delivery state and a photo it shows neither of.
    /// A board that says `board` walks only the 100, in pages of a serial
    /// line, each page saying honestly how many of them still wait; the
    /// phone, which says nothing, gets every entry as before. Control: the
    /// board's word dropped, the same walk brings the imported head first.
    #[test]
    fn a_board_is_served_what_it_folds_and_the_phone_everything() {
        use crate::home::board::{Batch, LinkClass};
        use crate::home::{KIND_DELIVERED, KIND_IMPORTED};
        let (_dir, mut store) = store();
        admitted(&mut store);
        let mut put = |kind: u8, n: u32, len: usize| {
            let mut id = [kind; 32];
            id[..4].copy_from_slice(&n.to_be_bytes());
            store
                .archive_append(kind, &id, &alloc::vec![7; len], 50, u64::MAX)
                .expect("append")
                .expect("new")
        };
        for n in 0..800 {
            put(KIND_IMPORTED, n, 150);
        }
        let real: Vec<u64> = (0..100).map(|n| put(KIND_RECEIVED, n, 150)).collect();
        put(KIND_DELIVERED, 0, 40);
        put(KIND_RECEIVED, 1_000, wire::BOARD_ENTRY_MAX + 1);
        let board = Batch {
            page_bytes: crate::home::ask::LORA_PAGE_BYTES as u64,
            max_answer: 256 * 1024,
        };
        let serial = LinkClass::Serial.page_bytes(crate::home::ask::SERIAL_LINK_MDU, None, board);
        assert_eq!(serial, 11_520, "a second of 115200 baud");
        // Each walk: the sequences, and per page how many it brought and
        // how many it said still wait.
        let walk = |store: &mut Store, fold: wire::Fold| {
            let (mut cursor, mut walked, mut pages) = (0, Vec::new(), Vec::new());
            loop {
                let data = wire::sync_request_folded(cursor, serial, fold);
                let answer = answer(store, &home(), SYNC_PATH, &data, Some(LAPTOP), 100.0);
                let page = read_sync_answer(&answer.bytes)
                    .expect("answer")
                    .expect("page");
                // One entry over the page comes alone, as before.
                assert!(answer.bytes.len() <= serial as usize || page.entries.len() == 1);
                walked.extend(page.entries.iter().map(|e| e.seq));
                pages.push((page.entries.len() as u64, page.left));
                let Some(last) = page.entries.last() else {
                    break;
                };
                cursor = last.seq;
                if !page.more {
                    break;
                }
            }
            (walked, pages)
        };
        let honest = |pages: &[(u64, Option<u64>)], all: u64| {
            let mut taken = 0;
            for &(n, left) in pages {
                taken += n;
                assert_eq!(left, Some(all - taken), "{pages:?}");
            }
            assert_eq!(taken, all);
        };
        let (walked, pages) = walk(&mut store, wire::Fold::Board);
        assert_eq!(walked, real, "the board gets the hundred, in order");
        honest(&pages, 100);
        assert!(pages.len() <= 3, "a serial page carries dozens: {pages:?}");
        // The phone says no word and gets all 902, the photo and the state
        // too, every page counting all of them.
        let (phone, pages) = walk(&mut store, wire::Fold::All);
        assert_eq!(phone.len(), 902);
        honest(&pages, 902);
        // Control: without the word the walk opens with the imported head.
        assert!(phone[..800].iter().all(|seq| !real.contains(seq)));
        let store_left = store
            .archive_count_after_folded(0, wire::Fold::Board)
            .expect("count");
        assert_eq!(store_left, 100);
        assert_eq!(store.archive_count_after(0).expect("count"), 902);
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
