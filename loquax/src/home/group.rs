//! A group hosted by its admin's home (1307,
//! `docs/drafts/group-chats-study-2026-11.md`).
//!
//! The group is an RNS identity of its own with an `lxmf.delivery`
//! destination; the home makes it and holds its private key, members never
//! do (study 2.1). A member writes to the group's address as to any
//! contact; the home proves the message once it is archived, so the
//! writer's proof means "the group has it", and sends every other present
//! member one copy **as the group**:
//!
//! - the content is `"<name>: <text>"` for every copy, the name the admin
//!   gave the writer: Columba keeps no title (study 1.3), so the content
//!   is the one place a name survives in every client;
//! - a member that showed capability bit 1 gets the `lev` key `0x02` too,
//!   the writer and the writer's own timestamp, the dedup pair, and the
//!   membership epoch; a stock member gets no `lev` pair;
//! - the copy's timestamp is the home's acceptance time, the group's order
//!   (study 6.1), the same for every copy;
//! - `FIELD_REPLY_TO` (0x30) is rewritten to the id of the recipient's own
//!   copy of the message replied to (its own original when it wrote it);
//!   attachments and the quote pass as they are.
//!
//! Every copy is a function of the original, the acceptance time, the
//! member and the group key alone (Ed25519 signs deterministically), so a
//! fan-out run again produces the same bytes (study 2.4). The copies and
//! their proofs are booked in `group_fanout` ([`crate::store::groups`]).
//!
//! Made from a device (pass two): `/group` ([`crate::home::server`]) makes
//! the group, the admin's invitations go through the home's outbox and the
//! membership to the admin through the group's engine ([`opening`]). The
//! admin removes a member by sending the group that member alone, flagged
//! left; the others are told "Lew removed Anna.", the removed one "You are
//! no longer in Familie.". A join tells every member that understands
//! groups the new list.
//!
//! G2 (1307 pass three):
//!
//! - a copy's retries survive a restart: [`rebuild`] makes an open copy
//!   again from the archived original, byte for byte (study 2.4);
//! - a member whose last [`SLOW_PROOFS`] proofs each took more than
//!   [`SLOW_PROOF_SECS`] is slow ([`measured`]); its lines are held and go
//!   as one digest per [`DIGEST_WINDOW_SECS`] ([`due_digests`]): the text
//!   `"Lew: …\nAnna: …"` for everyone, the `lev` key `0x06` for a Loquax
//!   member; an attachment above [`SLOW_ATTACHMENT_BYTES`] becomes the line
//!   "Lew sent a photo (too large for the radio)";
//! - a reaction goes to every other member that understands groups, its
//!   target rewritten to the recipient's own copy as a reply's is; a stock
//!   member gets none (decided), a slow one none either;
//! - a member who joins by writing (a stock member's first line) is told
//!   to the Loquax members with the membership, as a Join is; the admin
//!   adds members with [`add`].
//!
//! G3/G4 (1307 pass four):
//!
//! - a read receipt a member sends the group is booked against its copy
//!   (`group_fanout.read_at`); a member whose newest message said bit 0
//!   counts as a reader ([`MEMBER_RECEIPTS`]);
//! - the writer of a line that understands groups gets the `lev` key
//!   `0x03`, the counts of its copies ([`status_after_proof`],
//!   [`status_after_read`]): once every copy sent alone is proven, again
//!   for each late copy a digest proves, and once every reader read it;
//! - a copy a propagation node took is kept so (`propagated`), and a
//!   restart does not upload it again;
//! - the admin deletes the group ([`delete`]): every present member is
//!   told it ended, and the home forgets it.

use std::path::{Path, PathBuf};

use leviculum_core::Identity;
use leviculum_lxmf::constants::{
    FIELD_AUDIO, FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE, FIELD_FILE_ATTACHMENTS, FIELD_IMAGE,
    FIELD_REACTION, FIELD_REPLY_TO,
};
use leviculum_lxmf::lev::{
    DigestLine, GroupCopy, GroupStatus, Invite, Lev, Member, Members, CAP_GROUPS,
    CAP_READ_RECEIPTS, MEMBER_LEFT,
};
use leviculum_lxmf::message::Field;
use leviculum_lxmf::reaction::Reaction;
use leviculum_lxmf::reply::Reply;
use leviculum_lxmf::{DeliveryMethod, Message};

use crate::store::groups::{
    Copy, Counts, Group, GroupMember, COPY_PROVEN, MEMBER_ADMIN, MEMBER_GROUPS, MEMBER_JOINED,
    MEMBER_RECEIPTS, MEMBER_SLOW,
};
use crate::store::Store;
use crate::ui::strings::Lang;

/// The directory under the loquax home with the key of every group this
/// home hosts, one file a group named by its address in hex.
pub const GROUPS_DIR: &str = "groups";

/// What a stock member writes to leave (study 3.1); the invitation says so.
pub const LEAVE_WORD: &str = "/leave";

/// Where a stock member gets Loquax, as its invitation says (1307 pass
/// six). The site's page tree has `site/pages/download.md`, served at
/// `/download` once published; until then (2026-11-02 it answers 404)
/// this is the site's root, which answers.
pub const LOQUAX_URL: &str = "https://leviculum.network";

/// The most characters an invitation's text has (1307 pass six), its
/// `lxma://` code of 168 counted: a stock client shows it as one bubble.
pub const INVITATION_MAX_CHARS: usize = 299;

/// How long a slow member's lines are held before they go as one digest
/// (study 5.3), seconds.
pub const DIGEST_WINDOW_SECS: i64 = 60;

/// A proof slower than this, seconds, counts toward a slow member (study
/// 5.3: a copy and its proof over one LoRa hop at SF8 take 1.1 s, over the
/// internet tens of milliseconds).
pub const SLOW_PROOF_SECS: i64 = 2;

/// How many proofs in a row make a member slow, or fast again.
pub const SLOW_PROOFS: usize = 3;

/// The largest attachment a slow member still gets; above it the line
/// says what was sent instead (study 5.3).
pub const SLOW_ATTACHMENT_BYTES: usize = 2048;

/// A group this home hosts: its row and its identity, private key and all.
#[derive(Clone)]
pub struct Hosted {
    pub group: Group,
    pub identity: Identity,
}

/// One message the home sends: to whom, its LXMF id and its bytes, signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub destination: [u8; 16],
    pub id: [u8; 32],
    pub packed: Vec<u8>,
}

/// What a message to the group was, and what the home sends for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arrival {
    /// A line for the group: one copy per other present member.
    Line(Vec<Outgoing>),
    /// A member joined: the membership goes back to it when it
    /// understands groups.
    Joined(Vec<Outgoing>),
    /// A member left: the others are told.
    Left(Vec<Outgoing>),
    /// Nothing to show, but a receipt in it made a writer's status due
    /// (1307 pass four): the `0x03` for each.
    Status(Vec<Outgoing>),
    /// Proven and archived, nothing sent: a writer who is no member, a
    /// message with nothing to show (a receipt, a mark), a reaction nobody
    /// gets.
    Kept,
}

/// What [`book`] booked: the copy's group, and the originals whose copies
/// its proof proved first.
pub type Booked = ([u8; 16], Vec<[u8; 32]>);

/// Book one outbox change of a group copy in `group_fanout`: a delivery is
/// the member's proof (the writer's two ticks count these), the end of the
/// retries ends the copy, a propagation node taking it is kept (1307 pass
/// four), so a restart does not upload it again. `None` when `id` is no
/// copy of a group's, so the caller books it as any outbox message; else
/// the group and the originals of the copies this proof proved first,
/// whose writers' status may be due ([`status_after_proof`]).
pub fn book(
    store: &mut Store,
    id: &[u8; 32],
    change: crate::home::outgoing::Change,
    now: i64,
) -> Result<Option<Booked>, String> {
    use crate::home::outgoing::Change;
    let failed = |e: crate::store::StoreError| e.to_string();
    let Some(copy) = store
        .group_copy(id)
        .map_err(failed)?
        .or(store.group_digest_copy(id).map_err(failed)?)
    else {
        return Ok(None);
    };
    let digest = copy.copy != *id;
    let mut fresh = Vec::new();
    match change {
        Change::Delivered => {
            let carried = match digest {
                true => store.group_digest_copies(id).map_err(failed)?,
                false => vec![copy.clone()],
            };
            for carried in carried {
                if carried.state != COPY_PROVEN && !fresh.contains(&carried.original) {
                    fresh.push(carried.original);
                }
            }
            if digest {
                store.group_digest_changed(id, true, now).map_err(failed)?;
            } else {
                store.group_copy_proven(id, now).map_err(failed)?;
            }
            measured(store, &copy.group, &copy.member)?;
        }
        Change::Ended { .. } if digest => {
            store.group_digest_changed(id, false, now).map_err(failed)?;
        }
        Change::Ended { .. } => store.group_copy_ended(id).map_err(failed)?,
        // A digest's copies are held again at a restart and go out in the
        // next digest; only a copy sent alone is resumed as it was.
        Change::Propagated if !digest => {
            store.group_copy_propagated(id).map_err(failed)?;
        }
        Change::Propagated | Change::Stalled => {}
    }
    Ok(Some((copy.group, fresh)))
}

/// The status `original`'s writer gets after a copy of it was proven first
/// (1307 pass four, study 2.3): due once no copy sent alone waits for its
/// proof, which is the two ticks when no member is slow, and again for
/// every late copy a digest proves after that. `None` when not due, or
/// when the writer gets none ([`status`]).
pub fn status_after_proof(
    store: &Store,
    hosted: &Hosted,
    original: &[u8; 32],
    now: f64,
) -> Result<Option<Outgoing>, String> {
    let counts = store
        .group_counts(&hosted.group.address, original)
        .map_err(|e| e.to_string())?;
    match counts.waiting_alone {
        0 => status(store, hosted, original, counts, now),
        _ => Ok(None),
    }
}

/// The status `original`'s writer gets after a member read its copy first
/// (1307 pass four): due once every member that sends receipts read it.
pub fn status_after_read(
    store: &Store,
    hosted: &Hosted,
    original: &[u8; 32],
    now: f64,
) -> Result<Option<Outgoing>, String> {
    let counts = store
        .group_counts(&hosted.group.address, original)
        .map_err(|e| e.to_string())?;
    match counts.readers > 0 && counts.read >= counts.readers {
        true => status(store, hosted, original, counts, now),
        false => Ok(None),
    }
}

/// The `0x03` for the writer of `original` with `counts`: an empty message
/// from the group with the `lev` pair alone, which a Loquax keeps and does
/// not draw. Only a present writer that understands groups gets one (an
/// empty message is an empty notification in Sideband, study 1.3), and
/// only for a line: a reaction, a Join or a membership has no ticks.
fn status(
    store: &Store,
    hosted: &Hosted,
    original: &[u8; 32],
    counts: Counts,
    now: f64,
) -> Result<Option<Outgoing>, String> {
    let failed = |e: crate::store::StoreError| e.to_string();
    let Some(packed) = store
        .archive_entry(crate::home::KIND_GROUP, original)
        .map_err(failed)?
    else {
        return Ok(None);
    };
    let Ok(message) = Message::unpack(&packed, None, None, DeliveryMethod::Direct) else {
        return Ok(None);
    };
    if reaction_of(&message).is_some() || counts.members == 0 {
        return Ok(None);
    }
    let members = store.group_members(&hosted.group.address).map_err(failed)?;
    let Some(writer) = members
        .iter()
        .find(|m| m.address == message.source_hash && m.present() && m.understands_groups())
    else {
        return Ok(None);
    };
    let mut fields = Vec::new();
    Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        status: Some(GroupStatus {
            original: *original,
            delivered: counts.delivered,
            read: counts.read,
            members: counts.members,
            readers: counts.readers,
        }),
        ..Lev::default()
    }
    .put_into(&mut fields);
    tracing::info!(
        "GROUP {} status message={} delivered={}/{} read={}/{}",
        hosted.group.name,
        crate::address::to_hex(original),
        counts.delivered,
        counts.members,
        counts.read,
        counts.readers
    );
    sign(
        &hosted.identity,
        hosted.group.address,
        writer.address,
        now,
        Vec::new(),
        fields,
    )
    .map(Some)
}

/// Book the read receipt `original` carries, from a present member, against
/// that member's copies (1307 pass four: before, a receipt to the group was
/// kept and not booked), and the `0x03` each first read made due
/// ([`status_after_read`]). An id that is no copy of this member's in this
/// group is passed over.
fn read_receipts(
    store: &mut Store,
    hosted: &Hosted,
    original: &Message,
    lev: &Lev,
    now: f64,
) -> Result<Vec<Outgoing>, String> {
    let failed = |e: crate::store::StoreError| e.to_string();
    let Some(receipt) = &lev.receipt else {
        return Ok(Vec::new());
    };
    let group = hosted.group.address;
    let reader = original.source_hash;
    let mut read = Vec::new();
    for id in &receipt.ids {
        let ours = store
            .group_copy(id)
            .map_err(failed)?
            .is_some_and(|copy| copy.group == group && copy.member == reader);
        if !ours {
            continue;
        }
        if let Some(copy) = store
            .group_copy_read(id, &reader, i64::from(receipt.read_at))
            .map_err(failed)?
        {
            if !read.contains(&copy.original) {
                read.push(copy.original);
            }
        }
    }
    let mut out = Vec::new();
    for original in read {
        if let Some(status) = status_after_read(store, hosted, &original, now)? {
            out.push(status);
        }
    }
    Ok(out)
}

/// The admin deletes the group (1307 pass four): every present member is
/// told, a member that understands groups (the admin among them) with the
/// last membership, everyone flagged left and the standing ended, a stock
/// member with the text "Lew ended Familie."; then the group's rows, its
/// conversation in the archive and its key under `home` are forgotten.
/// What to send comes back, signed while the key was still here; the
/// caller's engine of the group sends it and refuses every later message.
pub fn delete(
    home: &Path,
    store: &mut Store,
    hosted: &Hosted,
    now: f64,
) -> Result<Vec<Outgoing>, String> {
    let failed = |e: crate::store::StoreError| e.to_string();
    let group = &hosted.group;
    let members = store.group_members(&group.address).map_err(failed)?;
    let epoch = store
        .group(&group.address)
        .map_err(failed)?
        .map_or(group.epoch, |g| g.epoch)
        + 1;
    let admin_name = members
        .iter()
        .find(|m| m.address == group.admin)
        .map_or_else(String::new, |m| m.name.clone());
    let gone: Vec<GroupMember> = members
        .iter()
        .map(|m| GroupMember {
            left_at: Some(m.left_at.unwrap_or(now as i64)),
            ..m.clone()
        })
        .collect();
    let ended = Members {
        ended: true,
        ..members_key(epoch, &gone)
    };
    let text = format!("{admin_name} ended {}.", group.name);
    let mut out = Vec::new();
    for member in members.iter().filter(|m| m.present()) {
        let mut fields = Vec::new();
        let content = match member.understands_groups() || member.address == group.admin {
            true => {
                Lev {
                    capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
                    members: Some(ended.clone()),
                    ..Lev::default()
                }
                .put_into(&mut fields);
                Vec::new()
            }
            false => text.clone().into_bytes(),
        };
        out.push(sign(
            &hosted.identity,
            group.address,
            member.address,
            now,
            content,
            fields,
        )?);
    }
    store.group_delete(&group.address).map_err(failed)?;
    let forgotten = store.archive_forget_group(&group.address).map_err(failed)?;
    match std::fs::remove_file(key_path(home, &group.address)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("group key: {error}")),
    }
    tracing::warn!(
        "GROUP {} deleted address={} told={} archive_forgotten={forgotten}",
        group.name,
        crate::address::to_hex(&group.address),
        out.len()
    );
    Ok(out)
}

/// Measure `member` after a proof (study 5.3): slow once each of its last
/// [`SLOW_PROOFS`] proofs took more than [`SLOW_PROOF_SECS`], fast again
/// once each took no more. Between the two it stays as it was, so one
/// odd proof moves nothing. Whether it is slow now.
pub fn measured(store: &mut Store, group: &[u8; 16], member: &[u8; 16]) -> Result<bool, String> {
    let failed = |e: crate::store::StoreError| e.to_string();
    let times = store
        .group_proof_times(group, member, SLOW_PROOFS)
        .map_err(failed)?;
    let was = store
        .group_members(group)
        .map_err(failed)?
        .iter()
        .any(|m| m.address == *member && m.flags & MEMBER_SLOW != 0);
    if times.len() < SLOW_PROOFS {
        return Ok(was);
    }
    if !was && times.iter().all(|t| *t > SLOW_PROOF_SECS) {
        store
            .group_flag(group, member, MEMBER_SLOW)
            .map_err(failed)?;
        tracing::info!("GROUP member {} is slow", crate::address::to_hex(member));
        return Ok(true);
    }
    if was && times.iter().all(|t| *t <= SLOW_PROOF_SECS) {
        store
            .group_unflag(group, member, MEMBER_SLOW)
            .map_err(failed)?;
        tracing::info!(
            "GROUP member {} is fast again",
            crate::address::to_hex(member)
        );
        return Ok(false);
    }
    Ok(was)
}

/// Every message an arrival sends.
impl Arrival {
    pub fn outgoing(self) -> Vec<Outgoing> {
        match self {
            Self::Line(out) | Self::Joined(out) | Self::Left(out) | Self::Status(out) => out,
            Self::Kept => Vec::new(),
        }
    }

    /// This arrival with the statuses `more` beside what it sends.
    fn and(self, more: Vec<Outgoing>) -> Self {
        if more.is_empty() {
            return self;
        }
        match self {
            Self::Line(mut out) => {
                out.extend(more);
                Self::Line(out)
            }
            Self::Joined(mut out) => {
                out.extend(more);
                Self::Joined(out)
            }
            Self::Left(mut out) => {
                out.extend(more);
                Self::Left(out)
            }
            Self::Status(mut out) => {
                out.extend(more);
                Self::Status(out)
            }
            Self::Kept => Self::Status(more),
        }
    }
}

/// Where a group's key is kept under `home`.
pub fn key_path(home: &Path, address: &[u8; 16]) -> PathBuf {
    home.join(GROUPS_DIR).join(crate::address::to_hex(address))
}

/// Make a group named `name` with `admin` (the home's own LXMF address,
/// `admin_name`) and `members`, each with the name the admin gives it: a
/// new identity, its key kept under `home`, its row and members in the
/// store at epoch 1. The admin understands groups and has joined.
pub fn create(
    home: &Path,
    store: &mut Store,
    name: &str,
    admin: ([u8; 16], &str),
    members: &[([u8; 16], String)],
    now: i64,
) -> Result<Hosted, String> {
    let identity = leviculum_std::generate_identity();
    let address = crate::home::lxmf_address(&identity.public_key_bytes())?;
    crate::identity::store(&key_path(home, &address), &identity).map_err(|e| e.to_string())?;
    let group = Group {
        address,
        name: name.to_string(),
        admin: admin.0,
        epoch: 1,
        created_at: now,
    };
    let mut rows = vec![GroupMember {
        flags: MEMBER_ADMIN | MEMBER_GROUPS | MEMBER_JOINED,
        ..GroupMember::new(admin.0, admin.1, now)
    }];
    rows.extend(
        members
            .iter()
            .filter(|(member, _)| *member != admin.0)
            .map(|(member, name)| GroupMember::new(*member, name, now)),
    );
    store
        .group_create(&group, &rows)
        .map_err(|e| e.to_string())?;
    Ok(Hosted { group, identity })
}

/// Every group the store holds whose key is under `home`. A group whose
/// key is missing or damaged is left out and said in the log: it cannot
/// prove or sign, so it cannot be hosted.
pub fn hosted(home: &Path, store: &Store) -> Result<Vec<Hosted>, String> {
    let mut out = Vec::new();
    for group in store.groups().map_err(|e| e.to_string())? {
        let path = key_path(home, &group.address);
        let identity = std::fs::read(&path)
            .ok()
            .and_then(|bytes| leviculum_core::identity_store::decode_identity(&bytes));
        match identity {
            Some(identity) => out.push(Hosted { group, identity }),
            None => tracing::warn!("GROUP no key for {} at {}", group.name, path.display()),
        }
    }
    Ok(out)
}

/// Whether `member` runs a client that shows a group as one (1307 pass
/// six): the newest message the home's owner has from it said bit 1, as
/// every Loquax says on every message. A member never heard is taken for
/// a stock client: the link in its invitation costs a Loquax nothing, its
/// card shows the group and not the text.
pub fn understands_groups(store: &Store, member: &[u8; 16]) -> Result<bool, String> {
    let newest = store
        .archive_newest_from(member)
        .map_err(|e| e.to_string())?;
    Ok(newest
        .and_then(|packed| Message::unpack(&packed, None, None, DeliveryMethod::Direct).ok())
        .and_then(|message| Lev::from_fields(&message.fields))
        .is_some_and(|lev| lev.supports_groups()))
}

/// The text of an invitation (1307 pass six), in the admin's catalogue
/// language: who invites to what, the group's `lxma://` code, how a stock
/// member leaves, and for a `stock` member once, where Loquax shows the
/// group as a group. Where long names would take the text past
/// [`INVITATION_MAX_CHARS`], the link sentence is left out.
pub fn invitation_text(
    admin_name: &str,
    group_name: &str,
    code: &str,
    lang: Lang,
    stock: bool,
) -> String {
    let base = match lang {
        Lang::En => {
            format!("{admin_name} invited you to {group_name}: {code} Reply {LEAVE_WORD} to leave.")
        }
        Lang::De => format!(
            "{admin_name} lädt dich zu {group_name} ein: {code} Antworte {LEAVE_WORD} zum Austreten."
        ),
    };
    if !stock {
        return base;
    }
    let link = match lang {
        Lang::En => format!(" Loquax shows members and names: {LOQUAX_URL}"),
        Lang::De => format!(" Loquax zeigt Mitglieder und Namen: {LOQUAX_URL}"),
    };
    match base.chars().count() + link.chars().count() <= INVITATION_MAX_CHARS {
        true => base + &link,
        false => base,
    }
}

/// The invitation the admin sends `member`: from the admin, a text any
/// client shows with the group's `lxma://` code in it, which Columba opens
/// as a contact, and the `lev` key `0x04` a Loquax shows as a card with
/// "Join" (study 4.2). Unknown `lev` keys are invisible to every stock
/// client, so the one message serves both; a `stock` member's text says
/// where Loquax is ([`invitation_text`]).
pub fn invitation(
    admin: &Identity,
    hosted: &Hosted,
    admin_name: &str,
    lang: Lang,
    member: [u8; 16],
    stock: bool,
    now: f64,
) -> Result<Outgoing, String> {
    let group = &hosted.group;
    let public_key = hosted.identity.public_key_bytes();
    let code = crate::lxma::encode_parts(&group.address, &public_key);
    let content = invitation_text(admin_name, &group.name, &code, lang, stock);
    let mut fields = Vec::new();
    Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        invite: Some(Invite {
            group: group.address,
            public_key,
            name: group.name.clone(),
            admin_name: admin_name.to_string(),
            epoch: group.epoch,
        }),
        ..Lev::default()
    }
    .put_into(&mut fields);
    sign(
        admin,
        group.admin,
        member,
        now,
        content.into_bytes(),
        fields,
    )
}

/// What a group made at `now` sends (1307 pass two): the invitation from
/// the admin to every other member, for the home's outbox, and the
/// membership from the group to the admin, for the group's own engine, so
/// every device of the admin learns the members from its archive.
pub fn opening(
    admin: &Identity,
    hosted: &Hosted,
    admin_name: &str,
    lang: Lang,
    store: &Store,
    now: f64,
) -> Result<(Vec<Outgoing>, Vec<Outgoing>), String> {
    let group = &hosted.group;
    let members = store
        .group_members(&group.address)
        .map_err(|e| e.to_string())?;
    let invitations = members
        .iter()
        .filter(|m| m.address != group.admin)
        .map(|m| {
            let stock = !(m.understands_groups() || understands_groups(store, &m.address)?);
            invitation(admin, hosted, admin_name, lang, m.address, stock, now)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let welcome = members
        .iter()
        .filter(|m| m.address == group.admin)
        .map(|m| membership(hosted, m, group.epoch, &members, now))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((invitations, welcome))
}

/// The group an invitation names, with the key that makes its address,
/// when `fields` carry one whose key does (1307 pass two): the home of an
/// invited member learns the group's identity from it, so a Join and
/// every line after it can be encrypted to the group before its announce
/// is heard.
pub fn invited_key(fields: &[Field]) -> Option<([u8; 16], [u8; 64])> {
    let invite = Lev::from_fields(fields)?.invite?;
    let address = crate::home::lxmf_address(&invite.public_key).ok()?;
    (address == invite.group).then_some((invite.group, invite.public_key))
}

/// Take in `original`, a message to the group the home has archived and
/// proves, accepted at `accepted_at` (unix seconds, the copies' timestamp),
/// and say what to send for it; the copies are booked in `group_fanout`
/// before they are returned, so a crash between the two leaves rows a run
/// again finds. A read receipt in it is booked too, and the statuses it
/// made due go with the rest (1307 pass four).
pub fn accept(
    store: &mut Store,
    hosted: &Hosted,
    original: &Message,
    accepted_at: f64,
) -> Result<Arrival, String> {
    let arrival = accept_message(store, hosted, original, accepted_at)?;
    let present = store
        .group_members(&hosted.group.address)
        .map_err(|e| e.to_string())?
        .iter()
        .any(|m| m.address == original.source_hash && m.present());
    if !present {
        return Ok(arrival);
    }
    let lev = Lev::from_fields(&original.fields).unwrap_or_default();
    let statuses = read_receipts(store, hosted, original, &lev, accepted_at)?;
    Ok(arrival.and(statuses))
}

fn accept_message(
    store: &mut Store,
    hosted: &Hosted,
    original: &Message,
    accepted_at: f64,
) -> Result<Arrival, String> {
    let group = hosted.group.address;
    let failed = |e: crate::store::StoreError| e.to_string();
    let members = store.group_members(&group).map_err(failed)?;
    let sender = original.source_hash;
    let Some(writer) = members
        .iter()
        .find(|m| m.address == sender && m.present())
        .cloned()
    else {
        // A stranger, or a member removed: proven, so its client stops
        // retrying, and dropped (study 4.6).
        return Ok(Arrival::Kept);
    };
    let said = Lev::from_fields(&original.fields);
    let lev = said.clone().unwrap_or_default();
    let mut joined = false;
    let mut flags = 0;
    if lev.supports_groups() && !writer.understands_groups() {
        flags |= MEMBER_GROUPS;
    }
    // Whether it sends read receipts, as its newest mark says (1307 pass
    // four); a message without the pair (a stock client's) says nothing.
    let reads = writer.flags & MEMBER_RECEIPTS != 0;
    if said.is_some() && lev.supports_receipts() && !reads {
        flags |= MEMBER_RECEIPTS;
    }
    if said.is_some() && !lev.supports_receipts() && reads {
        store
            .group_unflag(&group, &sender, MEMBER_RECEIPTS)
            .map_err(failed)?;
    }
    if writer.flags & MEMBER_JOINED == 0 {
        flags |= MEMBER_JOINED;
        joined = true;
    }
    if flags != 0 {
        store.group_flag(&group, &sender, flags).map_err(failed)?;
    }
    let understands = writer.understands_groups() || flags & MEMBER_GROUPS != 0;
    if let Some(removed) = removal(original, &lev, &hosted.group, &members) {
        let epoch = store
            .group_leave(&group, &removed.address, accepted_at as i64)
            .map_err(failed)?;
        let members = store.group_members(&group).map_err(failed)?;
        let line = format!("{} removed {}.", writer.name, removed.name);
        let mut told = members
            .iter()
            .filter(|m| m.present())
            .map(|m| notice(hosted, m, &line, epoch, &members, accepted_at))
            .collect::<Result<Vec<_>, _>>()?;
        let gone = members
            .iter()
            .find(|m| m.address == removed.address)
            .cloned()
            .unwrap_or(removed);
        let bye = format!("You are no longer in {}.", hosted.group.name);
        told.push(notice(hosted, &gone, &bye, epoch, &members, accepted_at)?);
        return Ok(Arrival::Left(told));
    }
    if is_leave(original, &lev) {
        let epoch = store
            .group_leave(&group, &sender, accepted_at as i64)
            .map_err(failed)?;
        let line = format!("{} left.", writer.name);
        let members = store.group_members(&group).map_err(failed)?;
        let told = members
            .iter()
            .filter(|m| m.present())
            .map(|m| notice(hosted, m, &line, epoch, &members, accepted_at))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Arrival::Left(told));
    }
    let shows = !original.content.is_empty()
        || original
            .fields
            .iter()
            .any(|(k, _)| [FIELD_IMAGE, FIELD_FILE_ATTACHMENTS, FIELD_AUDIO].contains(k));
    let reaction = reaction_of(original);
    let epoch = store
        .group(&group)
        .map_err(failed)?
        .map_or(hosted.group.epoch, |g| g.epoch);
    let members = store.group_members(&group).map_err(failed)?;
    // A member that joined with this message, by a Join or by writing: it
    // gets the list when it understands groups, and every other member
    // that does learns that it joined (pass two; a stock member who joins
    // by writing, G2).
    let told = match joined {
        true => members
            .iter()
            .filter(|m| {
                m.present()
                    && ((m.address == sender && understands)
                        || (m.address != sender && m.understands_groups()))
            })
            .map(|m| membership(hosted, m, epoch, &members, accepted_at))
            .collect::<Result<Vec<_>, _>>()?,
        false => Vec::new(),
    };
    if !shows && reaction.is_none() {
        return Ok(match told.is_empty() {
            true => Arrival::Kept,
            false => Arrival::Joined(told),
        });
    }
    let mut copies = Vec::new();
    let mut rows = Vec::new();
    for member in members
        .iter()
        .filter(|m| m.present() && m.address != sender)
    {
        // A reaction goes to the members that understand groups and are
        // not slow; a stock member gets none (decided for G2).
        if reaction.is_some() && (!member.understands_groups() || member.flags & MEMBER_SLOW != 0) {
            continue;
        }
        let out = copy_for(
            store,
            hosted,
            original,
            &writer.name,
            member,
            epoch,
            accepted_at,
        )?;
        let held =
            reaction.is_none() && member.flags & MEMBER_SLOW != 0 && !too_small_to_hold(original);
        rows.push(match held {
            true => Copy::held(
                group,
                original.message_id,
                member.address,
                out.id,
                accepted_at as i64,
            ),
            false => Copy::sent(
                group,
                original.message_id,
                member.address,
                out.id,
                accepted_at as i64,
            ),
        });
        if !held {
            copies.push(out);
        }
    }
    store.group_fanout_add(&rows).map_err(failed)?;
    copies.extend(told);
    Ok(match copies.is_empty() {
        true => Arrival::Kept,
        false => Arrival::Line(copies),
    })
}

/// The reaction `original` carries, when it is one.
fn reaction_of(original: &Message) -> Option<Reaction> {
    original
        .fields
        .iter()
        .find(|(k, _)| *k == FIELD_REACTION)
        .and_then(|(_, value)| Reaction::decode_field_value(value))
}

/// Whether a slow member gets `original` alone rather than in a digest: it
/// carries an attachment small enough to send (a digest carries text
/// only); a larger one goes into the digest as the line saying what it was.
fn too_small_to_hold(original: &Message) -> bool {
    let size = attachment_bytes(original);
    size > 0 && size <= SLOW_ATTACHMENT_BYTES
}

/// The bytes of `original`'s attachments: photo, files, voice.
fn attachment_bytes(original: &Message) -> usize {
    original
        .fields
        .iter()
        .filter(|(k, _)| [FIELD_IMAGE, FIELD_FILE_ATTACHMENTS, FIELD_AUDIO].contains(k))
        .map(|(_, value)| value.len())
        .sum()
}

/// The copy of `original` for `member` at `epoch`, its reply or reaction
/// target the id `member` knows the target by: what [`accept`] sends, and
/// what [`rebuild`] makes again.
fn copy_for(
    store: &Store,
    hosted: &Hosted,
    original: &Message,
    writer_name: &str,
    member: &GroupMember,
    epoch: u64,
    accepted_at: f64,
) -> Result<Outgoing, String> {
    let group = hosted.group.address;
    let sender = original.source_hash;
    // The message the writer replied or reacted to, as the group knows it:
    // the original of the copy the writer has, or an original itself (the
    // writer's own message).
    let target = Reply::from_fields(&original.fields)
        .map(|reply| reply.to)
        .or(reaction_of(original).map(|reaction| reaction.to));
    let target = match target {
        Some(to) => {
            let original_of = store
                .group_copy(&to)
                .map_err(|e| e.to_string())?
                .filter(|copy| copy.group == group && copy.member == sender)
                .map_or(to, |copy| copy.original);
            Some(reply_for(store, &group, &original_of, member.address)?)
        }
        None => None,
    };
    copy(
        hosted,
        original,
        writer_name,
        member,
        epoch,
        accepted_at,
        target,
    )
}

/// The copy `row` booked, made again from the archived original (1307 G2,
/// study 2.4), at a restart that lost its retries: the same bytes and id,
/// since a copy is a function of the original, the acceptance time, the
/// member, the epoch and the group key. The row keeps no epoch, so the
/// epochs are tried from the group's newest down and the one whose id is
/// the row's is taken. `None` when the original is no longer archived,
/// its writer or member is gone from the table, or no epoch makes the id.
pub fn rebuild(store: &Store, hosted: &Hosted, row: &Copy) -> Result<Option<Outgoing>, String> {
    let failed = |e: crate::store::StoreError| e.to_string();
    let Some(packed) = store
        .archive_entry(crate::home::KIND_GROUP, &row.original)
        .map_err(failed)?
    else {
        return Ok(None);
    };
    let original = Message::unpack(&packed, None, None, DeliveryMethod::Direct)
        .map_err(|e| format!("archived group message: {e:?}"))?;
    let members = store.group_members(&row.group).map_err(failed)?;
    let (Some(writer), Some(member)) = (
        members.iter().find(|m| m.address == original.source_hash),
        members.iter().find(|m| m.address == row.member),
    ) else {
        return Ok(None);
    };
    let newest = store
        .group(&row.group)
        .map_err(failed)?
        .map_or(hosted.group.epoch, |g| g.epoch);
    // The groups bit may have come after the copy went: try the member as
    // it is and as it was.
    let without = GroupMember {
        flags: member.flags & !MEMBER_GROUPS,
        ..member.clone()
    };
    for epoch in (1..=newest).rev() {
        for member in [member, &without] {
            let out = copy_for(
                store,
                hosted,
                &original,
                &writer.name,
                member,
                epoch,
                row.sent_at as f64,
            )?;
            if out.id == row.copy {
                return Ok(Some(out));
            }
        }
    }
    Ok(None)
}

/// The digests due at `now` (study 5.3): for every member with copies held
/// for [`DIGEST_WINDOW_SECS`] or longer (its oldest held one), one message
/// from the group with every held line, oldest first, and the held copies
/// it carries, for [`Store::group_digest_sent`]. A line whose original is
/// no longer archived is said as "a message (no longer here)".
pub fn due_digests(store: &Store, hosted: &Hosted, now: i64) -> Result<Vec<Digest>, String> {
    let failed = |e: crate::store::StoreError| e.to_string();
    let group = hosted.group.address;
    let held = store.group_copies_held(&group).map_err(failed)?;
    let members = store.group_members(&group).map_err(failed)?;
    let mut due: Vec<[u8; 16]> = Vec::new();
    for row in &held {
        if row.sent_at + DIGEST_WINDOW_SECS <= now && !due.contains(&row.member) {
            due.push(row.member);
        }
    }
    let mut out = Vec::new();
    for address in due {
        let Some(member) = members.iter().find(|m| m.address == address) else {
            continue;
        };
        let rows: Vec<&Copy> = held.iter().filter(|r| r.member == address).collect();
        let mut lines = Vec::new();
        for row in &rows {
            let packed = store
                .archive_entry(crate::home::KIND_GROUP, &row.original)
                .map_err(failed)?;
            let original = packed.and_then(|packed| {
                Message::unpack(&packed, None, None, DeliveryMethod::Direct).ok()
            });
            let line = match original {
                Some(original) => {
                    let name = members
                        .iter()
                        .find(|m| m.address == original.source_hash)
                        .map_or_else(String::new, |m| m.name.clone());
                    (
                        original.source_hash,
                        sent_ms(original.timestamp),
                        name,
                        digest_text(&original),
                    )
                }
                None => (
                    [0; 16],
                    0,
                    String::new(),
                    "a message (no longer here)".to_string(),
                ),
            };
            lines.push(line);
        }
        let text = lines
            .iter()
            .map(|(_, _, name, text)| match text.starts_with(SENT_WORD) {
                true => format!("{name} {text}"),
                false => format!("{name}: {text}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut fields = Vec::new();
        if member.understands_groups() {
            Lev {
                capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
                digest: Some(
                    lines
                        .iter()
                        .map(|(sender, sent_ms, _, text)| DigestLine {
                            sender: *sender,
                            sent_ms: *sent_ms,
                            text: text.clone(),
                        })
                        .collect(),
                ),
                ..Lev::default()
            }
            .put_into(&mut fields);
        }
        let digest = sign(
            &hosted.identity,
            group,
            address,
            now as f64,
            text.into_bytes(),
            fields,
        )?;
        out.push((digest, rows.iter().map(|r| r.copy).collect()));
    }
    Ok(out)
}

/// A digest for one slow member, and the held copies it carries.
pub type Digest = (Outgoing, Vec<[u8; 32]>);

/// How a refused attachment's line begins: "Lew sent a photo …".
const SENT_WORD: &str = "sent a ";

/// The words of `original` in a digest: its text, or for an attachment
/// above [`SLOW_ATTACHMENT_BYTES`] what it was, with its text after.
fn digest_text(original: &Message) -> String {
    let text = String::from_utf8_lossy(&original.content).into_owned();
    if attachment_bytes(original) <= SLOW_ATTACHMENT_BYTES {
        return text;
    }
    let has = |key: i64| original.fields.iter().any(|(k, _)| *k == key);
    let what = if has(FIELD_IMAGE) {
        "photo"
    } else if has(FIELD_AUDIO) {
        "voice message"
    } else {
        "file"
    };
    let said = format!("{SENT_WORD}{what} (too large for the radio)");
    match text.trim().is_empty() {
        true => said,
        false => format!("{said}: {text}"),
    }
}

/// The admin adds `members` to the group (study 4.5, 1307 G2): each is
/// kept, the epoch moves once per member, and what goes out comes back:
/// the invitations from the admin, for the home's outbox, and the new
/// membership from the group to every present member that understands
/// groups, for the group's engine. A member already present is skipped;
/// one that had left is invited again.
pub fn add(
    admin: &Identity,
    hosted: &Hosted,
    admin_name: &str,
    lang: Lang,
    store: &mut Store,
    members: &[([u8; 16], String)],
    now: f64,
) -> Result<(Vec<Outgoing>, Vec<Outgoing>), String> {
    let failed = |e: crate::store::StoreError| e.to_string();
    let group = hosted.group.address;
    let before = store.group_members(&group).map_err(failed)?;
    let mut added = Vec::new();
    for (address, name) in members {
        if *address == hosted.group.admin
            || before.iter().any(|m| m.address == *address && m.present())
            || added.contains(address)
        {
            continue;
        }
        store
            .group_add(&group, &GroupMember::new(*address, name, now as i64))
            .map_err(failed)?;
        added.push(*address);
    }
    if added.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let epoch = store
        .group(&group)
        .map_err(failed)?
        .map_or(hosted.group.epoch, |g| g.epoch);
    let current = Hosted {
        group: Group {
            epoch,
            ..hosted.group.clone()
        },
        identity: hosted.identity.clone(),
    };
    let invitations = added
        .iter()
        .map(|address| {
            let flagged = before
                .iter()
                .any(|m| m.address == *address && m.understands_groups());
            let stock = !(flagged || understands_groups(store, address)?);
            invitation(admin, &current, admin_name, lang, *address, stock, now)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let members = store.group_members(&group).map_err(failed)?;
    let told = members
        .iter()
        .filter(|m| m.present() && m.understands_groups())
        .map(|m| membership(hosted, m, epoch, &members, now))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((invitations, told))
}

/// The copy of `original` for `member`, as the module header says.
/// `reply_to` is the id the recipient knows the replied (or reacted-to)
/// message by; `None` drops the reply target, the quote stays, and drops
/// a reaction.
pub fn copy(
    hosted: &Hosted,
    original: &Message,
    writer_name: &str,
    member: &GroupMember,
    epoch: u64,
    accepted_at: f64,
    reply_to: Option<[u8; 32]>,
) -> Result<Outgoing, String> {
    let mut fields: Vec<Field> = Vec::new();
    for (key, value) in &original.fields {
        match *key {
            FIELD_CUSTOM_TYPE | FIELD_CUSTOM_DATA => {}
            FIELD_REPLY_TO => {
                if let Some(to) = reply_to {
                    let mut value = Vec::new();
                    leviculum_lxmf::msgpack::bin(&mut value, &to);
                    fields.push((FIELD_REPLY_TO, value));
                }
            }
            FIELD_REACTION => {
                // The reaction's target, the recipient's own copy (G2).
                if let (Some(to), Some(reaction)) = (reply_to, Reaction::decode_field_value(value))
                {
                    let value = Reaction { to, ..reaction }.encode_field_value();
                    fields.push((FIELD_REACTION, value));
                }
            }
            _ => fields.push((*key, value.clone())),
        }
    }
    if member.understands_groups() {
        let writer_caps = Lev::from_fields(&original.fields)
            .and_then(|lev| lev.capabilities)
            .unwrap_or(0);
        Lev {
            capabilities: Some(CAP_GROUPS | (writer_caps & CAP_READ_RECEIPTS)),
            group_copy: Some(GroupCopy {
                sender: original.source_hash,
                sent_ms: sent_ms(original.timestamp),
                epoch,
            }),
            ..Lev::default()
        }
        .put_into(&mut fields);
    }
    // A reaction is no line: its text (none, mostly) goes as it is.
    let content = match reaction_of(original) {
        Some(_) => original.content.clone(),
        None => prefixed(writer_name, &original.content),
    };
    sign(
        &hosted.identity,
        hosted.group.address,
        member.address,
        accepted_at,
        content,
        fields,
    )
}

/// `"<name>: <text>"`, and `"<name>:"` for a message without text (a
/// photo without caption still names its sender, study 3.1).
pub fn prefixed(name: &str, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2 + content.len());
    out.extend_from_slice(name.as_bytes());
    out.push(b':');
    if !content.is_empty() {
        out.push(b' ');
        out.extend_from_slice(content);
    }
    out
}

/// Whether `original` is its writer leaving: the `lev` membership listing
/// the writer alone, flagged left, or the text [`LEAVE_WORD`].
fn is_leave(original: &Message, lev: &Lev) -> bool {
    let listed = lev.members.as_ref().is_some_and(|members| {
        members.members.len() == 1
            && members.members[0].address == original.source_hash
            && members.members[0].flags & MEMBER_LEFT != 0
    });
    listed || original.content.trim_ascii() == LEAVE_WORD.as_bytes()
}

/// The member the admin removes with `original`: the `lev` membership from
/// the admin listing one other present member alone, flagged left
/// (study 4.6). A member that is not the admin removes nobody.
fn removal(
    original: &Message,
    lev: &Lev,
    group: &Group,
    members: &[GroupMember],
) -> Option<GroupMember> {
    if original.source_hash != group.admin {
        return None;
    }
    let listed = lev.members.as_ref()?;
    let [only] = listed.members.as_slice() else {
        return None;
    };
    if only.flags & MEMBER_LEFT == 0 || only.address == group.admin {
        return None;
    }
    members
        .iter()
        .find(|m| m.address == only.address && m.present())
        .cloned()
}

/// The id `member` knows `original` by: its copy's, or the original's own
/// when the member has no copy (it wrote it).
fn reply_for(
    store: &Store,
    group: &[u8; 16],
    original: &[u8; 32],
    member: [u8; 16],
) -> Result<[u8; 32], String> {
    Ok(store
        .group_copy_for(group, original, &member)
        .map_err(|e| e.to_string())?
        .map_or(*original, |copy| copy.copy))
}

/// The membership as the `lev` key `0x05` says it: every member, the left
/// ones flagged.
fn members_key(epoch: u64, members: &[GroupMember]) -> Members {
    Members {
        epoch,
        members: members
            .iter()
            .map(|m| Member {
                address: m.address,
                name: m.name.clone(),
                flags: if m.present() { 0 } else { MEMBER_LEFT }
                    | if m.flags & MEMBER_ADMIN != 0 {
                        leviculum_lxmf::lev::MEMBER_ADMIN
                    } else {
                        0
                    }
                    | if m.flags & MEMBER_JOINED != 0 {
                        leviculum_lxmf::lev::MEMBER_JOINED
                    } else {
                        0
                    },
            })
            .collect(),
        ended: false,
    }
}

/// The membership, from the group to `member`: an empty message with the
/// `lev` key `0x05`, only ever to a member that understands groups (an
/// empty message is an empty notification in Sideband, study 1.3).
fn membership(
    hosted: &Hosted,
    member: &GroupMember,
    epoch: u64,
    members: &[GroupMember],
    at: f64,
) -> Result<Outgoing, String> {
    let mut fields = Vec::new();
    Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        members: Some(members_key(epoch, members)),
        ..Lev::default()
    }
    .put_into(&mut fields);
    sign(
        &hosted.identity,
        hosted.group.address,
        member.address,
        at,
        Vec::new(),
        fields,
    )
}

/// A line of the group's own ("Anna left."), from the group to `member`;
/// with the membership in it for a member that understands groups.
fn notice(
    hosted: &Hosted,
    member: &GroupMember,
    line: &str,
    epoch: u64,
    members: &[GroupMember],
    at: f64,
) -> Result<Outgoing, String> {
    let mut fields = Vec::new();
    if member.understands_groups() {
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            members: Some(members_key(epoch, members)),
            ..Lev::default()
        }
        .put_into(&mut fields);
    }
    sign(
        &hosted.identity,
        hosted.group.address,
        member.address,
        at,
        line.as_bytes().to_vec(),
        fields,
    )
}

fn sign(
    identity: &Identity,
    source: [u8; 16],
    destination: [u8; 16],
    timestamp: f64,
    content: Vec<u8>,
    fields: Vec<Field>,
) -> Result<Outgoing, String> {
    let message = Message::create(
        destination,
        source,
        identity,
        timestamp,
        Vec::new(),
        content,
        fields,
        DeliveryMethod::Direct,
    )
    .map_err(|e| format!("group message: {e:?}"))?;
    Ok(Outgoing {
        destination,
        id: message.message_id,
        packed: message.pack(),
    })
}

/// An LXMF timestamp, seconds, as milliseconds; a time before 1970 or not
/// finite is 0.
fn sent_ms(timestamp: f64) -> u64 {
    if timestamp.is_finite() && timestamp > 0.0 {
        (timestamp * 1000.0).round() as u64
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::constants::FIELD_REPLY_QUOTE;
    use leviculum_lxmf::Verification;

    const ACCEPTED: f64 = 1_800_000_100.0;

    struct Scene {
        _home: tempfile::TempDir,
        store: Store,
        hosted: Hosted,
        lew: (Identity, [u8; 16]),
        anna: (Identity, [u8; 16]),
        bob: (Identity, [u8; 16]),
    }

    fn person() -> (Identity, [u8; 16]) {
        let identity = leviculum_std::generate_identity();
        let address = crate::home::lxmf_address(&identity.public_key_bytes()).expect("address");
        (identity, address)
    }

    /// Lew's home hosts "Familie": Lew the admin, Anna on Loquax (she has
    /// shown bit 1), Bob on a stock client.
    fn scene() -> Scene {
        let home = tempfile::tempdir().expect("home");
        let (lew, anna, bob) = (person(), person(), person());
        let mut store = Store::open_in(home.path(), lew.1).expect("store");
        let hosted = create(
            home.path(),
            &mut store,
            "Familie",
            (lew.1, "Lew"),
            &[(anna.1, "Anna".into()), (bob.1, "Bob".into())],
            100,
        )
        .expect("create");
        store
            .group_flag(&hosted.group.address, &anna.1, MEMBER_GROUPS)
            .expect("anna understands groups");
        Scene {
            _home: home,
            store,
            hosted,
            lew,
            anna,
            bob,
        }
    }

    fn to_group(
        scene: &Scene,
        from: &(Identity, [u8; 16]),
        timestamp: f64,
        content: &str,
        fields: Vec<Field>,
    ) -> Message {
        Message::create(
            scene.hosted.group.address,
            from.1,
            &from.0,
            timestamp,
            Vec::new(),
            content.as_bytes().to_vec(),
            fields,
            DeliveryMethod::Direct,
        )
        .expect("message")
    }

    fn opened(scene: &Scene, out: &Outgoing) -> Message {
        let message = Message::unpack(
            &out.packed,
            None,
            Some(&scene.hosted.identity),
            DeliveryMethod::Direct,
        )
        .expect("unpack");
        assert_eq!(
            message.verification,
            Verification::Valid,
            "signed by the group"
        );
        assert_eq!(message.source_hash, scene.hosted.group.address);
        assert_eq!(message.destination_hash, out.destination);
        assert_eq!(message.message_id, out.id);
        message
    }

    fn pick(scene: &Scene, copies: &[Outgoing], member: [u8; 16]) -> Message {
        opened(
            scene,
            copies
                .iter()
                .find(|c| c.destination == member)
                .expect("copy"),
        )
    }

    fn reply_of(message: &Message) -> Option<[u8; 32]> {
        Reply::from_fields(&message.fields).map(|r| r.to)
    }

    fn line(arrival: Arrival) -> Vec<Outgoing> {
        match arrival {
            Arrival::Line(copies) => copies,
            other => panic!("not a line: {other:?}"),
        }
    }

    #[test]
    fn a_line_goes_to_every_other_member_with_the_name_and_the_groups_time() {
        let mut scene = scene();
        // Anna joined before (a first line would join her, and the list
        // would go out with it: see the stock member's join below).
        scene
            .store
            .group_flag(&scene.hosted.group.address, &scene.anna.1, MEMBER_JOINED)
            .expect("joined");
        let mut fields = Vec::new();
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            ..Lev::default()
        }
        .put_into(&mut fields);
        let original = to_group(
            &scene,
            &scene.anna,
            1_800_000_000.25,
            "Bin gleich da, bringe Brot mit.",
            fields,
        );
        let hosted = scene.hosted.clone();
        let copies = line(accept(&mut scene.store, &hosted, &original, ACCEPTED).expect("accept"));
        let to: Vec<_> = copies.iter().map(|c| c.destination).collect();
        assert_eq!(to, [scene.lew.1, scene.bob.1], "everyone but the writer");
        for copy in &copies {
            let message = opened(&scene, copy);
            assert_eq!(message.content, b"Anna: Bin gleich da, bringe Brot mit.");
            assert_eq!(message.timestamp, ACCEPTED, "the home's acceptance time");
        }
        let lew = Lev::from_fields(&opened(&scene, &copies[0]).fields).expect("lev for Loquax");
        assert_eq!(
            lew.group_copy,
            Some(GroupCopy {
                sender: scene.anna.1,
                sent_ms: 1_800_000_000_250,
                epoch: 1,
            })
        );
        assert!(lew.supports_groups() && lew.supports_receipts());
        assert_eq!(
            Lev::from_fields(&opened(&scene, &copies[1]).fields),
            None,
            "no lev pair for the stock member"
        );
        let rows = scene
            .store
            .group_copies(&hosted.group.address, &original.message_id)
            .expect("rows");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| copies.iter().any(|c| c.id == r.copy)));
        // The same original at the same time is the same bytes, twice.
        let again = line(accept(&mut scene.store, &hosted, &original, ACCEPTED).expect("again"));
        assert_eq!(again, copies);
    }

    /// Bob answers Anna's line in Columba: his reply names his copy; Lew's
    /// copy of the answer names Lew's copy, Anna's names her own original.
    /// Then Anna answers Bob's answer.
    #[test]
    fn a_reply_names_each_recipients_own_copy() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let question = to_group(&scene, &scene.anna, 1_800_000_000.0, "Brot?", Vec::new());
        let asked = line(accept(&mut scene.store, &hosted, &question, ACCEPTED).expect("q"));
        let (lew_copy, bob_copy) = (asked[0].id, asked[1].id);
        let reply = Reply {
            to: bob_copy,
            quote: Some("Anna: Brot?".into()),
        };
        let answer = to_group(&scene, &scene.bob, 1_800_000_050.0, "Ja!", reply.fields());
        let answered =
            line(accept(&mut scene.store, &hosted, &answer, ACCEPTED + 1.0).expect("answer"));
        let to_lew = pick(&scene, &answered, scene.lew.1);
        let to_anna = pick(&scene, &answered, scene.anna.1);
        assert_eq!(reply_of(&to_lew), Some(lew_copy));
        assert_eq!(
            reply_of(&to_anna),
            Some(question.message_id),
            "her own original"
        );
        assert_eq!(to_anna.content, b"Bob: Ja!");
        for message in [&to_lew, &to_anna] {
            let quote = message.fields.iter().find(|(k, _)| *k == FIELD_REPLY_QUOTE);
            assert!(quote.is_some(), "the quote passes");
        }
        // Anna answers Bob's answer by her copy of it.
        let anna_copy = answered
            .iter()
            .find(|c| c.destination == scene.anna.1)
            .expect("anna's")
            .id;
        let again = Reply {
            to: anna_copy,
            quote: None,
        };
        let third = to_group(&scene, &scene.anna, 1_800_000_060.0, "Gut.", again.fields());
        let thirded = line(accept(&mut scene.store, &hosted, &third, ACCEPTED + 2.0).expect("3"));
        let lew_answer = answered
            .iter()
            .find(|c| c.destination == scene.lew.1)
            .expect("lew's")
            .id;
        assert_eq!(
            reply_of(&pick(&scene, &thirded, scene.lew.1)),
            Some(lew_answer)
        );
        assert_eq!(
            reply_of(&pick(&scene, &thirded, scene.bob.1)),
            Some(answer.message_id),
            "Bob's own original"
        );
    }

    #[test]
    fn a_join_a_leave_and_a_stranger() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let group = hosted.group.address;
        // Bob, a stock client, writes first: he joins by writing, and his
        // line goes out.
        let hello = to_group(&scene, &scene.bob, 1.0, "Hallo", Vec::new());
        assert!(matches!(
            accept(&mut scene.store, &hosted, &hello, ACCEPTED).expect("hello"),
            Arrival::Line(_)
        ));
        // Anna's Join: an empty message with bit 1. The membership goes
        // back to her and to Lew, who understands groups (pass two), not
        // to Bob's stock client.
        let mut fields = Vec::new();
        Lev {
            capabilities: Some(CAP_GROUPS),
            ..Lev::default()
        }
        .put_into(&mut fields);
        let join = to_group(&scene, &scene.anna, 2.0, "", fields);
        let Arrival::Joined(back) =
            accept(&mut scene.store, &hosted, &join, ACCEPTED).expect("join")
        else {
            panic!("not a join");
        };
        let to: Vec<_> = back.iter().map(|b| b.destination).collect();
        assert_eq!(to, [scene.lew.1, scene.anna.1]);
        let back = &back[1..];
        let members = Lev::from_fields(&opened(&scene, &back[0]).fields)
            .and_then(|lev| lev.members)
            .expect("membership");
        assert_eq!(members.epoch, 1);
        let names: Vec<_> = members.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"Lew") && names.contains(&"Anna") && names.contains(&"Bob"));
        let joined = scene.store.group_members(&group).expect("members");
        assert!(joined.iter().all(|m| m.flags & MEMBER_JOINED != 0));
        // Again: nothing to send.
        assert_eq!(
            accept(&mut scene.store, &hosted, &join, ACCEPTED).expect("again"),
            Arrival::Kept
        );
        // Bob writes /leave: Lew and Anna are told, Anna with the list.
        let leave = to_group(&scene, &scene.bob, 3.0, " /leave ", Vec::new());
        let Arrival::Left(told) = accept(&mut scene.store, &hosted, &leave, ACCEPTED).expect("l")
        else {
            panic!("not a leave");
        };
        let to: Vec<_> = told.iter().map(|t| t.destination).collect();
        assert_eq!(to, [scene.lew.1, scene.anna.1]);
        assert_eq!(opened(&scene, &told[1]).content, b"Bob left.");
        let list = Lev::from_fields(&opened(&scene, &told[1]).fields)
            .and_then(|lev| lev.members)
            .expect("list");
        assert_eq!(list.epoch, 2);
        assert!(list
            .members
            .iter()
            .any(|m| m.name == "Bob" && m.flags & MEMBER_LEFT != 0));
        // Gone, Bob's next line is kept and sent to nobody; so is a stranger's.
        let after = to_group(&scene, &scene.bob, 4.0, "Noch da?", Vec::new());
        assert_eq!(
            accept(&mut scene.store, &hosted, &after, ACCEPTED).expect("after"),
            Arrival::Kept
        );
        let stranger = person();
        let spam = to_group(&scene, &stranger, 5.0, "Hi", Vec::new());
        assert_eq!(
            accept(&mut scene.store, &hosted, &spam, ACCEPTED).expect("spam"),
            Arrival::Kept
        );
        // Anna leaves by the lev list, herself flagged left.
        let mut fields = Vec::new();
        Lev {
            capabilities: Some(CAP_GROUPS),
            members: Some(Members {
                epoch: 2,
                members: vec![Member {
                    address: scene.anna.1,
                    name: "Anna".into(),
                    flags: MEMBER_LEFT,
                }],
                ended: false,
            }),
            ..Lev::default()
        }
        .put_into(&mut fields);
        let gone = to_group(&scene, &scene.anna, 6.0, "", fields);
        let Arrival::Left(told) = accept(&mut scene.store, &hosted, &gone, ACCEPTED).expect("a")
        else {
            panic!("not a leave");
        };
        assert_eq!(told.len(), 1, "Lew alone is left to tell");
    }

    /// 1307 pass two: the admin removes Anna by sending the group her alone,
    /// flagged left. The others are told who removed whom, Anna that she is
    /// no longer in it, with the list for both who understand groups; her
    /// next line goes nowhere. Controls: the same list from Bob or Anna,
    /// who are no admins, removes nobody. (The admin listing himself is
    /// his leave, as any member's.)
    #[test]
    fn the_admin_removes_a_member_and_nobody_else_can() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let group = hosted.group.address;
        let list = |who: [u8; 16], name: &str| {
            let mut fields = Vec::new();
            Lev {
                // No capability: Bob's stock client stays stock.
                capabilities: None,
                members: Some(Members {
                    epoch: 1,
                    members: vec![Member {
                        address: who,
                        name: name.into(),
                        flags: MEMBER_LEFT,
                    }],
                    ended: false,
                }),
                ..Lev::default()
            }
            .put_into(&mut fields);
            fields
        };
        let by_bob = to_group(&scene, &scene.bob, 1.0, "", list(scene.anna.1, "Anna"));
        assert!(!matches!(
            accept(&mut scene.store, &hosted, &by_bob, ACCEPTED).expect("bob"),
            Arrival::Left(_)
        ));
        let by_anna = to_group(&scene, &scene.anna, 2.0, "", list(scene.bob.1, "Bob"));
        assert!(!matches!(
            accept(&mut scene.store, &hosted, &by_anna, ACCEPTED).expect("anna"),
            Arrival::Left(_)
        ));
        assert!(scene
            .store
            .group_members(&group)
            .expect("members")
            .iter()
            .all(GroupMember::present));

        let removal = to_group(&scene, &scene.lew, 3.0, "", list(scene.anna.1, "Anna"));
        let Arrival::Left(told) =
            accept(&mut scene.store, &hosted, &removal, ACCEPTED).expect("removal")
        else {
            panic!("not a removal");
        };
        let to: Vec<_> = told.iter().map(|t| t.destination).collect();
        assert_eq!(to, [scene.lew.1, scene.bob.1, scene.anna.1]);
        assert_eq!(opened(&scene, &told[1]).content, b"Lew removed Anna.");
        assert!(
            Lev::from_fields(&opened(&scene, &told[1]).fields).is_none(),
            "Bob is stock"
        );
        let bye = opened(&scene, &told[2]);
        assert_eq!(bye.content, b"You are no longer in Familie.");
        let members = Lev::from_fields(&bye.fields)
            .and_then(|lev| lev.members)
            .expect("Anna's list");
        assert_eq!(members.epoch, 2);
        assert!(members
            .members
            .iter()
            .any(|m| m.address == scene.anna.1 && m.flags & MEMBER_LEFT != 0));
        let after = to_group(&scene, &scene.anna, 4.0, "Hallo?", Vec::new());
        assert_eq!(
            accept(&mut scene.store, &hosted, &after, ACCEPTED).expect("after"),
            Arrival::Kept
        );
    }

    /// 1307 pass two: a group's opening is one invitation a member from the
    /// admin and the membership from the group to the admin; the key an
    /// invitation carries is learned only when it makes the group's
    /// address (control: a key of someone else's is not).
    #[test]
    fn a_new_group_invites_and_tells_its_admin() {
        let scene = scene();
        let (invitations, welcome) = opening(
            &scene.lew.0,
            &scene.hosted,
            "Lew",
            Lang::En,
            &scene.store,
            7.0,
        )
        .expect("opening");
        let to: Vec<_> = invitations.iter().map(|i| i.destination).collect();
        assert_eq!(to, [scene.anna.1, scene.bob.1]);
        assert_eq!(welcome.len(), 1);
        let welcome = opened(&scene, &welcome[0]);
        assert_eq!(welcome.destination_hash, scene.lew.1);
        assert!(welcome.content.is_empty());
        let list = Lev::from_fields(&welcome.fields)
            .and_then(|lev| lev.members)
            .expect("list");
        let admin = list
            .members
            .iter()
            .find(|m| m.address == scene.lew.1)
            .expect("Lew");
        assert_eq!(
            admin.flags,
            leviculum_lxmf::lev::MEMBER_ADMIN | leviculum_lxmf::lev::MEMBER_JOINED
        );
        let card = Message::unpack(&invitations[0].packed, None, None, DeliveryMethod::Direct)
            .expect("card");
        assert_eq!(
            invited_key(&card.fields),
            Some((
                scene.hosted.group.address,
                scene.hosted.identity.public_key_bytes()
            ))
        );
        let mut forged = Lev::from_fields(&card.fields).expect("lev");
        if let Some(invite) = forged.invite.as_mut() {
            invite.public_key = scene.bob.0.public_key_bytes();
        }
        let mut fields = Vec::new();
        forged.put_into(&mut fields);
        assert_eq!(invited_key(&fields), None);
    }

    /// 1307 pass six: a stock member's invitation says once where Loquax
    /// shows the group as a group, in the admin's language, and stays
    /// under 300 characters with its `lxma://` code; a Loquax member's is
    /// as it was. Names too long for the link leave it out, never the
    /// code.
    #[test]
    fn a_stock_members_invitation_says_where_loquax_is_and_stays_short() {
        let scene = scene();
        let code = crate::lxma::encode_parts(
            &scene.hosted.group.address,
            &scene.hosted.identity.public_key_bytes(),
        );
        assert_eq!(code.chars().count(), 168);
        let en = invitation_text("Lew", "Familie", &code, Lang::En, true);
        assert_eq!(
            en,
            format!(
                "Lew invited you to Familie: {code} Reply /leave to leave. \
                 Loquax shows members and names: https://leviculum.network"
            )
        );
        let de = invitation_text("Lew", "Familie", &code, Lang::De, true);
        assert_eq!(
            de,
            format!(
                "Lew lädt dich zu Familie ein: {code} Antworte /leave zum Austreten. \
                 Loquax zeigt Mitglieder und Namen: https://leviculum.network"
            )
        );
        for text in [&en, &de] {
            assert!(
                text.chars().count() < 300,
                "{}: {text}",
                text.chars().count()
            );
            assert_eq!(text.matches(LOQUAX_URL).count(), 1, "once");
        }
        // Control: a Loquax member's text is the one it was.
        assert_eq!(
            invitation_text("Lew", "Familie", &code, Lang::En, false),
            format!("Lew invited you to Familie: {code} Reply /leave to leave.")
        );
        assert!(!invitation_text("Lew", "Familie", &code, Lang::De, false).contains(LOQUAX_URL));
        let long = invitation_text("Lew", "Sportverein Hinterdorf e.V.", &code, Lang::De, true);
        assert!(long.contains(&code) && !long.contains(LOQUAX_URL), "{long}");
        assert!(long.chars().count() < 300);

        // The opening: Anna showed bit 1 to the group, Bob is stock.
        let (invitations, _) = opening(
            &scene.lew.0,
            &scene.hosted,
            "Lew",
            Lang::De,
            &scene.store,
            7.0,
        )
        .expect("opening");
        let text = |to: [u8; 16]| {
            let out = invitations
                .iter()
                .find(|i| i.destination == to)
                .expect("invited");
            let message =
                Message::unpack(&out.packed, None, None, DeliveryMethod::Direct).expect("unpack");
            String::from_utf8(message.content).expect("text")
        };
        assert_eq!(text(scene.bob.1), de);
        assert_eq!(
            text(scene.anna.1),
            invitation_text("Lew", "Familie", &code, Lang::De, false)
        );
    }

    /// 1307 pass six: whether a member runs Loquax is what its newest
    /// message to the owner said; a member never heard is stock.
    #[test]
    fn a_member_whose_newest_message_said_bit_one_understands_groups() {
        let mut scene = scene();
        let cara = person();
        assert!(!understands_groups(&scene.store, &cara.1).expect("never heard"));
        let said = |caps: Option<u64>, at: f64| {
            let mut fields = Vec::new();
            Lev {
                capabilities: caps,
                ..Lev::default()
            }
            .put_into(&mut fields);
            Message::create(
                scene.lew.1,
                cara.1,
                &cara.0,
                at,
                Vec::new(),
                b"Hallo".to_vec(),
                fields,
                DeliveryMethod::Direct,
            )
            .expect("message")
        };
        let loquax = said(Some(CAP_GROUPS), 1.0);
        scene
            .store
            .archive_append(
                crate::home::KIND_RECEIVED,
                &loquax.message_id,
                &loquax.pack(),
                1,
                u64::MAX,
            )
            .expect("archive");
        assert!(understands_groups(&scene.store, &cara.1).expect("Loquax"));
        assert!(!understands_groups(&scene.store, &scene.bob.1).expect("someone else"));
        let columba = said(None, 2.0);
        scene
            .store
            .archive_append(
                crate::home::KIND_RECEIVED,
                &columba.message_id,
                &columba.pack(),
                2,
                u64::MAX,
            )
            .expect("archive");
        assert!(
            !understands_groups(&scene.store, &cara.1).expect("now stock"),
            "the newest decides"
        );
    }

    #[test]
    fn the_invitation_carries_the_code_and_the_card() {
        let scene = scene();
        let out = invitation(
            &scene.lew.0,
            &scene.hosted,
            "Lew",
            Lang::En,
            scene.bob.1,
            true,
            7.0,
        )
        .expect("invitation");
        let message = Message::unpack(
            &out.packed,
            None,
            Some(&scene.lew.0),
            DeliveryMethod::Direct,
        )
        .expect("unpack");
        assert_eq!(message.verification, Verification::Valid);
        assert_eq!(message.source_hash, scene.lew.1, "from the admin");
        let text = String::from_utf8(message.content.clone()).expect("text");
        let code = crate::lxma::encode_parts(
            &scene.hosted.group.address,
            &scene.hosted.identity.public_key_bytes(),
        );
        assert!(
            text.starts_with("Lew invited you to Familie: lxma://"),
            "{text}"
        );
        assert!(text.contains(&code) && text.contains("/leave"), "{text}");
        let contact = crate::lxma::parse(&code).expect("Columba reads the code");
        assert_eq!(contact.address, scene.hosted.group.address);
        let invite = Lev::from_fields(&message.fields)
            .and_then(|lev| lev.invite)
            .expect("the card");
        assert_eq!(invite.group, scene.hosted.group.address);
        assert_eq!(invite.public_key, scene.hosted.identity.public_key_bytes());
        assert_eq!(
            (invite.name.as_str(), invite.admin_name.as_str()),
            ("Familie", "Lew")
        );
    }

    #[test]
    fn a_hosted_group_comes_back_with_its_key_and_a_lost_key_leaves_it_out() {
        let scene = scene();
        let home = scene._home.path();
        let back = hosted(home, &scene.store).expect("hosted");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].group, scene.hosted.group);
        assert_eq!(
            back[0].identity.public_key_bytes(),
            scene.hosted.identity.public_key_bytes()
        );
        std::fs::remove_file(key_path(home, &scene.hosted.group.address)).expect("rm");
        assert!(hosted(home, &scene.store).expect("hosted").is_empty());
    }

    #[test]
    fn a_name_goes_in_front_of_every_text_and_alone_before_a_photo() {
        assert_eq!(prefixed("Lew", b"Hallo"), b"Lew: Hallo");
        assert_eq!(prefixed("Lew", b""), b"Lew:");
    }

    fn archived(scene: &mut Scene, message: &Message) {
        scene
            .store
            .archive_append(
                crate::home::KIND_GROUP,
                &message.message_id,
                &message.pack(),
                ACCEPTED as i64,
                u64::MAX,
            )
            .expect("archive");
    }

    fn groups_mark() -> Vec<Field> {
        let mut fields = Vec::new();
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            ..Lev::default()
        }
        .put_into(&mut fields);
        fields
    }

    /// 1307 G2, item 1: a copy whose retries a restart cut off is made
    /// again from the archived original, byte for byte, a reply's rewritten
    /// target and all, though the epoch moved since (Bob left after it).
    /// Controls: a proven copy is not open; a row whose time is not the
    /// acceptance time makes no copy (the id is checked, not trusted).
    #[test]
    fn a_copy_a_restart_cut_off_is_made_again_byte_for_byte() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let group = hosted.group.address;
        let bob_line = to_group(&scene, &scene.bob, 1.0, "Wer holt Brot?", Vec::new());
        archived(&mut scene, &bob_line);
        let first = line(accept(&mut scene.store, &hosted, &bob_line, ACCEPTED).expect("bob"));
        let annas = first
            .iter()
            .find(|c| c.destination == scene.anna.1)
            .expect("anna's copy")
            .id;
        let mut fields = groups_mark();
        fields.extend(
            Reply {
                to: annas,
                quote: None,
            }
            .fields(),
        );
        let reply = to_group(&scene, &scene.anna, 2.0, "Ich", fields);
        archived(&mut scene, &reply);
        let sent = line(accept(&mut scene.store, &hosted, &reply, ACCEPTED + 1.0).expect("anna"));
        // Bob leaves: the epoch moves to 2 after Anna's reply went at 1.
        let leave = to_group(&scene, &scene.bob, 3.0, LEAVE_WORD, Vec::new());
        accept(&mut scene.store, &hosted, &leave, ACCEPTED + 2.0).expect("leave");
        assert_eq!(scene.store.group(&group).expect("g").expect("g").epoch, 2);
        // Lew's copy of Bob's line is proven; everything else is open.
        let lews_of_bob = first
            .iter()
            .find(|c| c.destination == scene.lew.1)
            .expect("lew")
            .id;
        scene
            .store
            .group_copy_proven(&lews_of_bob, ACCEPTED as i64 + 5)
            .expect("proven");
        let open = scene.store.group_copies_open(&group).expect("open");
        assert_eq!(open.len(), 3, "Anna's of Bob's, Lew's and Bob's of Anna's");
        assert!(open.iter().all(|row| row.copy != lews_of_bob));
        for row in &open {
            let again = rebuild(&scene.store, &hosted, row)
                .expect("rebuild")
                .expect("made again");
            let before = first
                .iter()
                .chain(&sent)
                .find(|c| c.id == row.copy)
                .expect("sent before");
            assert_eq!(&again, before, "byte for byte");
        }
        let lews_reply = pick(&scene, &sent, scene.lew.1);
        assert_eq!(
            reply_of(&lews_reply),
            Some(open_copy(&scene, &first, scene.lew.1))
        );
        let shifted = Copy {
            sent_at: open[0].sent_at + 1,
            ..open[0].clone()
        };
        assert_eq!(
            rebuild(&scene.store, &hosted, &shifted).expect("rebuild"),
            None
        );
    }

    fn open_copy(_scene: &Scene, copies: &[Outgoing], member: [u8; 16]) -> [u8; 32] {
        copies
            .iter()
            .find(|c| c.destination == member)
            .expect("copy")
            .id
    }

    /// Book `n` proofs of `member` that took `secs` each, on lines of
    /// Lew's accepted from `at` on.
    fn proofs(scene: &mut Scene, member: [u8; 16], n: usize, secs: i64, at: f64) {
        let hosted = scene.hosted.clone();
        for i in 0..n {
            let accepted = at + i as f64 * 10.0;
            let message = to_group(
                scene,
                &scene.lew,
                accepted,
                &format!("Zeile {accepted}"),
                groups_mark(),
            );
            archived(scene, &message);
            accept(&mut scene.store, &hosted, &message, accepted).expect("line");
            let copy = scene
                .store
                .group_copy_for(&hosted.group.address, &message.message_id, &member)
                .expect("row")
                .expect("copy");
            assert!(book(
                &mut scene.store,
                &copy.copy,
                crate::home::outgoing::Change::Delivered,
                copy.sent_at + secs,
            )
            .expect("book")
            .is_some());
        }
    }

    fn slow(scene: &Scene, member: [u8; 16]) -> bool {
        scene
            .store
            .group_members(&scene.hosted.group.address)
            .expect("members")
            .iter()
            .any(|m| m.address == member && m.flags & MEMBER_SLOW != 0)
    }

    /// 1307 G2, item 4 (study 5.3): three proofs over 2 s make a member
    /// slow; its lines are held and go after 60 s as one digest, the text
    /// for every member and `0x06` for a Loquax one; a photo above 2 KB is
    /// the line saying so, one at 1 KB goes alone at once. Controls: two
    /// slow proofs make nobody slow; at 59 s nothing is due; the digest's
    /// proof proves every line in it; three fast digests make it fast.
    #[test]
    fn a_slow_member_gets_its_lines_as_one_digest() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let group = hosted.group.address;
        let (anna, bob) = (scene.anna.1, scene.bob.1);
        proofs(&mut scene, anna, 2, 5, ACCEPTED);
        proofs(&mut scene, bob, 2, 5, ACCEPTED);
        assert!(
            !slow(&scene, anna) && !slow(&scene, bob),
            "two proofs are no measure"
        );
        proofs(&mut scene, anna, 1, 5, ACCEPTED + 100.0);
        proofs(&mut scene, bob, 1, 2, ACCEPTED + 100.0);
        assert!(slow(&scene, anna), "three proofs over 2 s");
        assert!(!slow(&scene, bob), "2 s is not over 2 s");
        proofs(&mut scene, bob, 2, 3, ACCEPTED + 200.0);
        assert!(!slow(&scene, bob), "one fast proof among the last three");
        proofs(&mut scene, bob, 1, 3, ACCEPTED + 300.0);
        assert!(slow(&scene, bob));

        // What the measuring lines held goes first, so the window below
        // is the one under test.
        flush(&mut scene, i64::MAX / 2);
        let t = ACCEPTED + 1000.0;
        let eins = to_group(&scene, &scene.lew, t, "eins", groups_mark());
        let mut big = groups_mark();
        let mut photo = Vec::new();
        leviculum_lxmf::msgpack::bin(&mut photo, &[7u8; 3000]);
        big.push((FIELD_IMAGE, photo));
        let zwei = to_group(&scene, &scene.lew, t + 10.0, "zwei", big);
        let mut small = groups_mark();
        let mut thumb = Vec::new();
        leviculum_lxmf::msgpack::bin(&mut thumb, &[7u8; 1000]);
        small.push((FIELD_IMAGE, thumb));
        let drei = to_group(&scene, &scene.lew, t + 20.0, "", small);
        for (i, m) in [&eins, &zwei, &drei].into_iter().enumerate() {
            archived(&mut scene, m);
            let out = accept(&mut scene.store, &hosted, m, t + 10.0 * i as f64).expect("accept");
            let to: Vec<[u8; 16]> = out.outgoing().iter().map(|o| o.destination).collect();
            match i {
                2 => assert_eq!(to, [anna, bob], "the small photo goes alone"),
                _ => assert!(to.is_empty(), "held: {to:?}"),
            }
        }
        assert!(due_digests(&scene.store, &hosted, t as i64 + 59)
            .expect("due")
            .is_empty());
        let due = due_digests(&scene.store, &hosted, t as i64 + 60).expect("due");
        assert_eq!(due.len(), 2, "one digest each");
        let text = "Lew: eins\nLew sent a photo (too large for the radio): zwei";
        for (out, copies) in &due {
            assert_eq!(copies.len(), 2);
            let message = opened(&scene, out);
            assert_eq!(String::from_utf8_lossy(&message.content), text);
            let lev = Lev::from_fields(&message.fields);
            if out.destination == anna {
                let lines = lev.and_then(|l| l.digest).expect("0x06 for Loquax");
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].sender, scene.lew.1);
                assert_eq!(lines[0].text, "eins");
                assert_eq!(lines[0].sent_ms, (t * 1000.0) as u64);
            } else {
                assert_eq!(lev, None, "text only for the stock member");
            }
        }
        let (bobs, bob_copies) = due.iter().find(|(o, _)| o.destination == bob).expect("bob");
        scene
            .store
            .group_digest_sent(bob_copies, &bobs.id, t as i64 + 60)
            .expect("sent");
        assert!(
            due_digests(&scene.store, &hosted, t as i64 + 120)
                .expect("due")
                .iter()
                .all(|(o, _)| o.destination == anna),
            "bob's lines went"
        );
        assert!(book(
            &mut scene.store,
            &bobs.id,
            crate::home::outgoing::Change::Delivered,
            t as i64 + 61
        )
        .expect("book")
        .is_some());
        for original in [&eins, &zwei] {
            let row = scene
                .store
                .group_copy_for(&group, &original.message_id, &bob)
                .expect("row")
                .expect("copy");
            assert_eq!(row.state, crate::store::groups::COPY_PROVEN);
            assert_eq!(row.digest, Some(bobs.id));
        }
        // Digests proven at once: with the one above (1 s) three fast
        // proofs make Bob fast again, and a line goes to him alone; after
        // the first of them he is still slow.
        flush(&mut scene, i64::MAX / 2);
        for i in 0..2 {
            assert!(slow(&scene, bob), "not yet fast before digest {i}");
            let at = t + 1000.0 + 100.0 * i as f64;
            let m = to_group(&scene, &scene.lew, at, "schnell", groups_mark());
            archived(&mut scene, &m);
            accept(&mut scene.store, &hosted, &m, at).expect("line");
            let due = due_digests(&scene.store, &hosted, at as i64 + 60).expect("due");
            let (out, copies) = due
                .iter()
                .find(|(o, _)| o.destination == bob)
                .expect("bob's");
            scene
                .store
                .group_digest_sent(copies, &out.id, at as i64 + 60)
                .expect("sent");
            book(
                &mut scene.store,
                &out.id,
                crate::home::outgoing::Change::Delivered,
                at as i64 + 60,
            )
            .expect("book");
        }
        assert!(!slow(&scene, bob), "three fast proofs");
        let m = to_group(&scene, &scene.lew, t + 2000.0, "allein", groups_mark());
        let out = accept(&mut scene.store, &hosted, &m, t + 2000.0).expect("line");
        assert!(out.outgoing().iter().any(|o| o.destination == bob));
    }

    /// Every digest due by `now` goes, booked as sent.
    fn flush(scene: &mut Scene, now: i64) {
        let hosted = scene.hosted.clone();
        for (out, copies) in due_digests(&scene.store, &hosted, now).expect("due") {
            scene
                .store
                .group_digest_sent(&copies, &out.id, 0)
                .expect("sent");
        }
    }

    fn reaction(to: [u8; 32]) -> Vec<Field> {
        let mut fields = groups_mark();
        fields.push((
            FIELD_REACTION,
            Reaction {
                to,
                content: "\u{1F44D}".into(),
            }
            .encode_field_value(),
        ));
        fields
    }

    /// 1307 G2, item 5: a reaction goes to the other members that
    /// understand groups, its target each one's own copy (or own original),
    /// its text without a name in front; a stock member gets none, by the
    /// decision. A stock member's reaction reaches the Loquax members too.
    #[test]
    fn a_reaction_names_each_loquax_members_own_copy_and_skips_stock() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let anna_line = to_group(&scene, &scene.anna, 1.0, "Brot ist da", groups_mark());
        let copies = line(accept(&mut scene.store, &hosted, &anna_line, ACCEPTED).expect("line"));
        let lews = open_copy(&scene, &copies, scene.lew.1);
        let bobs = open_copy(&scene, &copies, scene.bob.1);
        let thumbs = to_group(&scene, &scene.lew, 2.0, "", reaction(lews));
        let out = line(accept(&mut scene.store, &hosted, &thumbs, ACCEPTED + 1.0).expect("react"));
        let to: Vec<_> = out.iter().map(|o| o.destination).collect();
        assert_eq!(to, [scene.anna.1], "Anna, not Bob's stock client");
        let annas = opened(&scene, &out[0]);
        assert!(annas.content.is_empty(), "no name in front of a reaction");
        let read = annas
            .fields
            .iter()
            .find(|(k, _)| *k == FIELD_REACTION)
            .and_then(|(_, v)| Reaction::decode_field_value(v))
            .expect("reaction");
        assert_eq!(read.to, anna_line.message_id, "her own original");
        let lev = Lev::from_fields(&annas.fields).expect("lev");
        assert_eq!(
            lev.group_copy.map(|c| c.sender),
            Some(scene.lew.1),
            "who reacted"
        );
        // Bob reacts from his stock client to his copy: Lew's names Lew's
        // copy, Anna's her original; Bob is joined by it.
        let bob_thumbs = to_group(
            &scene,
            &scene.bob,
            3.0,
            "",
            vec![(
                FIELD_REACTION,
                Reaction {
                    to: bobs,
                    content: "\u{2764}".into(),
                }
                .encode_field_value(),
            )],
        );
        let out =
            line(accept(&mut scene.store, &hosted, &bob_thumbs, ACCEPTED + 2.0).expect("bob"));
        let targets: Vec<([u8; 16], Option<[u8; 32]>)> = out
            .iter()
            .map(|o| {
                let m = opened(&scene, o);
                let to = m
                    .fields
                    .iter()
                    .find(|(k, _)| *k == FIELD_REACTION)
                    .and_then(|(_, v)| Reaction::decode_field_value(v))
                    .map(|r| r.to);
                (o.destination, to)
            })
            .collect();
        assert!(targets.contains(&(scene.lew.1, Some(lews))), "{targets:?}");
        assert!(targets.contains(&(scene.anna.1, Some(anna_line.message_id))));
        assert!(targets.iter().all(|(to, _)| *to != scene.bob.1));
    }

    /// 1307 G2, item 6: a stock member who joins by writing is told to the
    /// Loquax members with the membership, as a Join is: Bob's first line
    /// goes out with the list to Lew and Anna, Bob flagged joined. Control:
    /// his second line carries no list.
    #[test]
    fn a_stock_member_who_joins_by_writing_is_told_to_the_loquax_members() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let hello = to_group(&scene, &scene.bob, 1.0, "Hallo", Vec::new());
        let out = line(accept(&mut scene.store, &hosted, &hello, ACCEPTED).expect("hello"));
        let lists: Vec<[u8; 16]> = out
            .iter()
            .filter(|o| {
                Lev::from_fields(&opened(&scene, o).fields).is_some_and(|l| l.members.is_some())
            })
            .map(|o| o.destination)
            .collect();
        assert_eq!(lists, [scene.lew.1, scene.anna.1]);
        let members = Lev::from_fields(&opened(&scene, &out[out.len() - 1]).fields)
            .and_then(|l| l.members)
            .expect("list");
        let bob = members
            .members
            .iter()
            .find(|m| m.address == scene.bob.1)
            .expect("bob");
        assert_ne!(bob.flags & leviculum_lxmf::lev::MEMBER_JOINED, 0);
        let again = to_group(&scene, &scene.bob, 2.0, "Noch da", Vec::new());
        let out = line(accept(&mut scene.store, &hosted, &again, ACCEPTED + 1.0).expect("again"));
        assert_eq!(out.len(), 2, "the two copies, no list");
    }

    /// 1307 G2, item 6 (study 4.5): the admin adds Cara: she is invited
    /// from the admin, the epoch moves, and every present member that
    /// understands groups gets the list with her in it. Control: adding a
    /// present member again sends nothing and moves nothing.
    #[test]
    fn the_admin_adds_a_member_who_is_invited_and_told() {
        let mut scene = scene();
        let hosted = scene.hosted.clone();
        let group = hosted.group.address;
        let cara = person();
        let (invites, told) = add(
            &scene.lew.0,
            &hosted,
            "Lew",
            Lang::En,
            &mut scene.store,
            &[(cara.1, "Cara".into()), (scene.anna.1, "Anna".into())],
            ACCEPTED,
        )
        .expect("add");
        assert_eq!(invites.len(), 1);
        assert_eq!(invites[0].destination, cara.1);
        let invite = Message::unpack(&invites[0].packed, None, None, DeliveryMethod::Direct)
            .expect("invite");
        assert_eq!(invite.source_hash, scene.lew.1, "from the admin");
        let card = Lev::from_fields(&invite.fields)
            .and_then(|l| l.invite)
            .expect("card");
        assert_eq!((card.group, card.epoch), (group, 2));
        let to: Vec<_> = told.iter().map(|o| o.destination).collect();
        assert_eq!(to, [scene.lew.1, scene.anna.1]);
        let list = Lev::from_fields(&opened(&scene, &told[1]).fields)
            .and_then(|l| l.members)
            .expect("list");
        assert_eq!(list.epoch, 2);
        assert!(list
            .members
            .iter()
            .any(|m| m.address == cara.1 && m.name == "Cara"));
        let (invites, told) = add(
            &scene.lew.0,
            &hosted,
            "Lew",
            Lang::En,
            &mut scene.store,
            &[(scene.anna.1, "Anna".into())],
            ACCEPTED,
        )
        .expect("again");
        assert!(invites.is_empty() && told.is_empty());
        assert_eq!(scene.store.group(&group).expect("g").expect("g").epoch, 2);
    }

    fn receipts_mark() -> Vec<Field> {
        let mut fields = Vec::new();
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            ..Lev::default()
        }
        .put_into(&mut fields);
        fields
    }

    fn status_of(scene: &Scene, out: &Outgoing) -> GroupStatus {
        let message = opened(scene, out);
        assert!(message.content.is_empty(), "a status is no line");
        Lev::from_fields(&message.fields)
            .and_then(|lev| lev.status)
            .expect("0x03")
    }

    /// 1307 pass four, the writer's ticks (study 2.3): Anna's line has two
    /// copies; the first proof makes no status, the last makes one to
    /// Anna, every copy proven, nobody a reader yet. Lew's read receipt to
    /// the group is booked against his copy (before, it was kept and not
    /// booked), makes him a reader, and sends Anna "read by 1 of 1".
    /// Controls: a proof booked twice proves nothing first; a receipt
    /// naming another member's copy is not booked; a stock writer gets no
    /// status; a receipt from a member whose mark says no receipts any more
    /// is booked, and the member counts as no reader.
    #[test]
    fn the_writer_hears_when_every_copy_is_proven_and_when_every_reader_read() {
        let mut scene = scene();
        let group = scene.hosted.group.address;
        let hosted = scene.hosted.clone();
        scene
            .store
            .group_flag(&group, &scene.anna.1, MEMBER_JOINED)
            .expect("joined");
        let original = to_group(
            &scene,
            &scene.anna,
            1_800_000_000.0,
            "Brot",
            receipts_mark(),
        );
        archived(&mut scene, &original);
        let copies = line(accept(&mut scene.store, &hosted, &original, ACCEPTED).expect("line"));
        let (lews, bobs) = (copies[0].id, copies[1].id);
        let first = book(
            &mut scene.store,
            &lews,
            crate::home::outgoing::Change::Delivered,
            200,
        )
        .expect("book")
        .expect("a copy");
        assert_eq!(first, (group, vec![original.message_id]));
        assert_eq!(
            status_after_proof(&scene.store, &hosted, &original.message_id, ACCEPTED).expect("s"),
            None,
            "Bob's copy waits"
        );
        let again = book(
            &mut scene.store,
            &lews,
            crate::home::outgoing::Change::Delivered,
            201,
        )
        .expect("book")
        .expect("a copy");
        assert!(again.1.is_empty(), "control: proven before");
        book(
            &mut scene.store,
            &bobs,
            crate::home::outgoing::Change::Delivered,
            202,
        )
        .expect("book");
        let two = status_after_proof(&scene.store, &hosted, &original.message_id, ACCEPTED)
            .expect("s")
            .expect("due");
        assert_eq!(two.destination, scene.anna.1, "to the writer");
        assert_eq!(
            status_of(&scene, &two),
            GroupStatus {
                original: original.message_id,
                delivered: 2,
                read: 0,
                members: 2,
                readers: 0,
            }
        );

        // Lew reads it: his client's receipt to the group names his copy.
        let mut fields = Vec::new();
        Lev::mark_with(leviculum_lxmf::lev::Receipt {
            read_at: 1_800_000_050,
            ids: vec![lews, bobs],
        })
        .put_into(&mut fields);
        let mut lev = Lev::from_fields(&fields).expect("lev");
        lev.capabilities = Some(CAP_READ_RECEIPTS | CAP_GROUPS);
        lev.put_into(&mut fields);
        let receipt = to_group(&scene, &scene.lew, 1_800_000_060.0, "", fields);
        let Arrival::Status(statuses) =
            accept(&mut scene.store, &hosted, &receipt, ACCEPTED + 60.0).expect("receipt")
        else {
            panic!("a status for the receipt");
        };
        let [read] = statuses.as_slice() else {
            panic!("one status: {statuses:?}");
        };
        assert_eq!(read.destination, scene.anna.1);
        let said = status_of(&scene, read);
        assert_eq!((said.read, said.readers), (1, 1), "read by 1 of 1");
        let rows = scene
            .store
            .group_copies(&group, &original.message_id)
            .expect("rows");
        let read_at = |id: [u8; 32]| rows.iter().find(|r| r.copy == id).and_then(|r| r.read_at);
        assert_eq!(read_at(lews), Some(1_800_000_050), "booked");
        assert_eq!(read_at(bobs), None, "control: Bob's copy is Bob's to read");

        // Control: a stock writer (Bob) gets no status.
        let stock = to_group(&scene, &scene.bob, 1_800_000_100.0, "Danke", Vec::new());
        archived(&mut scene, &stock);
        let copies = line(accept(&mut scene.store, &hosted, &stock, ACCEPTED + 100.0).expect("l"));
        for copy in &copies {
            book(
                &mut scene.store,
                &copy.id,
                crate::home::outgoing::Change::Delivered,
                300,
            )
            .expect("book");
        }
        assert_eq!(
            status_after_proof(&scene.store, &hosted, &stock.message_id, ACCEPTED).expect("s"),
            None
        );

        // Control: Lew switches receipts off; he counts as no reader.
        let mut off_mark = Vec::new();
        Lev {
            capabilities: Some(CAP_GROUPS),
            ..Lev::default()
        }
        .put_into(&mut off_mark);
        let off = to_group(&scene, &scene.lew, 1_800_000_200.0, "still here", off_mark);
        accept(&mut scene.store, &hosted, &off, ACCEPTED + 200.0).expect("off");
        let counts = scene
            .store
            .group_counts(&group, &original.message_id)
            .expect("counts");
        assert_eq!((counts.read, counts.readers), (0, 0));
    }

    /// 1307 pass four, the owner deletes "Familie": Lew and Anna (Loquax)
    /// get the last membership, everyone flagged left and the standing
    /// ended; Bob (stock) the text "Lew ended Familie."; then the rows, the
    /// key file and the group's archived lines are gone and the home hosts
    /// nothing. Control: another group of the same home is untouched.
    #[test]
    fn a_deleted_group_tells_its_members_and_leaves_nothing_behind() {
        let mut scene = scene();
        let group = scene.hosted.group.address;
        let hosted = scene.hosted.clone();
        let line_ = to_group(&scene, &scene.anna, 1_800_000_000.0, "Brot", Vec::new());
        archived(&mut scene, &line_);
        accept(&mut scene.store, &hosted, &line_, ACCEPTED).expect("line");
        let home = scene._home.path().to_path_buf();
        let other = create(
            &home,
            &mut scene.store,
            "Verein",
            (scene.lew.1, "Lew"),
            &[(scene.bob.1, "Bob".into())],
            100,
        )
        .expect("another group");
        let told = delete(&home, &mut scene.store, &hosted, ACCEPTED).expect("delete");
        let to: Vec<[u8; 16]> = told.iter().map(|o| o.destination).collect();
        assert_eq!(to, [scene.lew.1, scene.anna.1, scene.bob.1]);
        for out in &told[..2] {
            let message = opened(&scene, out);
            assert!(message.content.is_empty());
            let members = Lev::from_fields(&message.fields)
                .and_then(|lev| lev.members)
                .expect("the last membership");
            assert!(members.ended);
            assert_eq!(members.epoch, 2);
            assert!(members.members.iter().all(|m| m.flags & MEMBER_LEFT != 0));
        }
        let bob = opened(&scene, &told[2]);
        assert_eq!(bob.content, b"Lew ended Familie.");
        assert_eq!(Lev::from_fields(&bob.fields), None);
        assert_eq!(scene.store.group(&group).expect("g"), None);
        assert!(scene.store.group_members(&group).expect("m").is_empty());
        assert!(scene
            .store
            .group_copies(&group, &line_.message_id)
            .expect("c")
            .is_empty());
        assert_eq!(
            scene
                .store
                .archive_entry(crate::home::KIND_GROUP, &line_.message_id)
                .expect("a"),
            None
        );
        assert!(!key_path(&home, &group).exists(), "the key is gone");
        let left: Vec<[u8; 16]> = hosted_groups(&home, &scene.store);
        assert_eq!(left, [other.group.address], "control: Verein stays");
    }

    fn hosted_groups(home: &Path, store: &Store) -> Vec<[u8; 16]> {
        hosted(home, store)
            .expect("hosted")
            .iter()
            .map(|h| h.group.address)
            .collect()
    }
}
