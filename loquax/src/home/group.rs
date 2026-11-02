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
//! Not here yet (later slices of the order): reactions are not fanned out,
//! read receipts to the group are not booked, slow members get no digest.

use std::path::{Path, PathBuf};

use leviculum_core::Identity;
use leviculum_lxmf::constants::{
    FIELD_AUDIO, FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE, FIELD_FILE_ATTACHMENTS, FIELD_IMAGE,
    FIELD_REACTION, FIELD_REPLY_TO,
};
use leviculum_lxmf::lev::{
    GroupCopy, Invite, Lev, Member, Members, CAP_GROUPS, CAP_READ_RECEIPTS, MEMBER_LEFT,
};
use leviculum_lxmf::message::Field;
use leviculum_lxmf::reply::Reply;
use leviculum_lxmf::{DeliveryMethod, Message};

use crate::store::groups::{Copy, Group, GroupMember, MEMBER_ADMIN, MEMBER_GROUPS, MEMBER_JOINED};
use crate::store::Store;

/// The directory under the loquax home with the key of every group this
/// home hosts, one file a group named by its address in hex.
pub const GROUPS_DIR: &str = "groups";

/// What a stock member writes to leave (study 3.1); the invitation says so.
pub const LEAVE_WORD: &str = "/leave";

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
    /// Proven and archived, nothing sent: a writer who is no member, a
    /// message with nothing to show (a receipt, a mark), a reaction.
    Kept,
}

/// Book one outbox change of a group copy in `group_fanout`: a delivery is
/// the member's proof (the two ticks of slice G2 count these), the end of
/// the retries ends the copy. `false` when `id` is no copy of a group's,
/// so the caller books it as any outbox message.
pub fn book(
    store: &mut Store,
    id: &[u8; 32],
    change: crate::home::outgoing::Change,
    now: i64,
) -> Result<bool, String> {
    use crate::home::outgoing::Change;
    if store.group_copy(id).map_err(|e| e.to_string())?.is_none() {
        return Ok(false);
    }
    match change {
        Change::Delivered => {
            store
                .group_copy_proven(id, now)
                .map_err(|e| e.to_string())?;
        }
        Change::Ended { .. } => store.group_copy_ended(id).map_err(|e| e.to_string())?,
        Change::Propagated | Change::Stalled => {}
    }
    Ok(true)
}

/// Every message an arrival sends.
impl Arrival {
    pub fn outgoing(self) -> Vec<Outgoing> {
        match self {
            Self::Line(out) | Self::Joined(out) | Self::Left(out) => out,
            Self::Kept => Vec::new(),
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

/// The invitation the admin sends `member`: from the admin, a text any
/// client shows with the group's `lxma://` code in it, which Columba opens
/// as a contact, and the `lev` key `0x04` a Loquax shows as a card with
/// "Join" (study 4.2). Unknown `lev` keys are invisible to every stock
/// client, so the one message serves both.
pub fn invitation(
    admin: &Identity,
    hosted: &Hosted,
    admin_name: &str,
    member: [u8; 16],
    now: f64,
) -> Result<Outgoing, String> {
    let group = &hosted.group;
    let public_key = hosted.identity.public_key_bytes();
    let code = crate::lxma::encode_parts(&group.address, &public_key);
    let content = format!(
        "{admin_name} invited you to {}: {code} Reply {LEAVE_WORD} to leave.",
        group.name
    );
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

/// Take in `original`, a message to the group the home has archived and
/// proves, accepted at `accepted_at` (unix seconds, the copies' timestamp),
/// and say what to send for it; the copies are booked in `group_fanout`
/// before they are returned, so a crash between the two leaves rows a run
/// again finds.
pub fn accept(
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
    let lev = Lev::from_fields(&original.fields).unwrap_or_default();
    let mut joined = false;
    let mut flags = 0;
    if lev.supports_groups() && !writer.understands_groups() {
        flags |= MEMBER_GROUPS;
    }
    if writer.flags & MEMBER_JOINED == 0 {
        flags |= MEMBER_JOINED;
        joined = true;
    }
    if flags != 0 {
        store.group_flag(&group, &sender, flags).map_err(failed)?;
    }
    let understands = writer.understands_groups() || flags & MEMBER_GROUPS != 0;
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
    let reaction = original.fields.iter().any(|(k, _)| *k == FIELD_REACTION);
    if !shows || reaction {
        if joined && understands {
            let group_row = store.group(&group).map_err(failed)?;
            let epoch = group_row.map_or(hosted.group.epoch, |g| g.epoch);
            let members = store.group_members(&group).map_err(failed)?;
            let writer = members
                .iter()
                .find(|m| m.address == sender)
                .cloned()
                .unwrap_or(writer);
            return Ok(Arrival::Joined(vec![membership(
                hosted,
                &writer,
                epoch,
                &members,
                accepted_at,
            )?]));
        }
        return Ok(Arrival::Kept);
    }
    let epoch = store
        .group(&group)
        .map_err(failed)?
        .map_or(hosted.group.epoch, |g| g.epoch);
    // The message the writer replied to, as the group knows it: the
    // original of the copy the writer has, or an original itself (the
    // writer's own message).
    let replied = match Reply::from_fields(&original.fields) {
        Some(reply) => Some(
            store
                .group_copy(&reply.to)
                .map_err(failed)?
                .filter(|copy| copy.group == group && copy.member == sender)
                .map_or(reply.to, |copy| copy.original),
        ),
        None => None,
    };
    let mut copies = Vec::new();
    let mut rows = Vec::new();
    for member in members
        .iter()
        .filter(|m| m.present() && m.address != sender)
    {
        let reply_to = match replied {
            Some(replied) => Some(reply_for(store, &group, &replied, member.address)?),
            None => None,
        };
        let copy = copy(
            hosted,
            original,
            &writer.name,
            member,
            epoch,
            accepted_at,
            reply_to,
        )?;
        rows.push(Copy::sent(
            group,
            original.message_id,
            member.address,
            copy.id,
            accepted_at as i64,
        ));
        copies.push(copy);
    }
    store.group_fanout_add(&rows).map_err(failed)?;
    Ok(Arrival::Line(copies))
}

/// The copy of `original` for `member`, as the module header says.
/// `reply_to` is the id the recipient knows the replied message by, when
/// the original replies and the recipient has it; `None` drops the reply
/// target, the quote stays.
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
    sign(
        &hosted.identity,
        hosted.group.address,
        member.address,
        accepted_at,
        prefixed(writer_name, &original.content),
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
                    },
            })
            .collect(),
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
        // back to her and to no one else.
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
        assert_eq!(back.len(), 1);
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

    #[test]
    fn the_invitation_carries_the_code_and_the_card() {
        let scene = scene();
        let out =
            invitation(&scene.lew.0, &scene.hosted, "Lew", scene.bob.1, 7.0).expect("invitation");
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
}
