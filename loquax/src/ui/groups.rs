//! Group chats on a device (1307 pass two,
//! `docs/drafts/group-chats-study-2026-11.md` sections 4 and 5): what the
//! model knows of the groups its reader is in, and the reader's verbs on
//! them. A group lives at its admin's home; the device only ever writes to
//! the group's address, as to any contact.
//!
//! What the model knows comes from the messages themselves, so the store
//! needs no table of its own and every device of a home learns the same
//! from the archive:
//!
//! - an invitation (`lev 0x04`) names the group, its key and its admin;
//!   received, the reader is invited; sent (the admin's own, synced from
//!   the home), the reader made it;
//! - a membership (`0x05`) from a group the reader knows that way gives
//!   the members, who joined, who left and who is admin, the newest epoch
//!   winning; the reader's own entry says whether it is in; a list from
//!   any other address is a stranger's word and taken for nothing;
//! - a copy (`0x02`) names its writer, so a bubble that is not the
//!   reader's carries the writer's name above it and its text without the
//!   home's `"Name: "` prefix.
//!
//! A message that is only a group's state (a Join, a Leave, a membership,
//! all of them empty with the `lev` pair alone) is no bubble
//! ([`Message::is_group_state`]); it stays in the conversation and the
//! store, so a restart learns it again.
//!
//! The verbs ([`Verb`]) are the study's flows: "New group", tick the
//! friends, Next, the typed name and Create make the group through the
//! home (`/group`, [`UiEffect::CreateGroup`]); "Join" on the invitation
//! card sends the group an empty message with capability bit 1; the member
//! sheet's "Leave group" sends the group the reader alone, flagged left,
//! and the admin's "Remove" sends it the removed member alone, flagged
//! left. Leave and Remove ask once.
//!
//! 1307 pass four: the home's status (`0x03`) moves the reader's own line
//! to read once every member that sends receipts read it; until then the
//! group's proof is one tick (the group has it). 1466, the ticks tell the
//! truth in a group too: every member's copy proven is one tick, "Arrived"
//! (a member's mailbox proves with the member's identity, so a proof is no
//! device's word), and two ticks, "On the devices", come only once every
//! member's device said it has its copy (`0x07`, or its read receipt). A
//! member whose device never says it keeps the line at one tick, and the
//! Info names each member's rung and since when ([`UiModel::group_rungs`]). The admin deletes the group from
//! the member sheet ("Delete group", asked once, through the home,
//! `/groupdelete`): the admin's devices forget it whole, a member's keep
//! the history, closed, with the line "Lew ended the group." A group the
//! reader left or that ended can be started again with the same people.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_lxmf::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE};
use leviculum_lxmf::lev::{
    DigestLine, GroupStatus, Lev, Member, MemberRung, Members, StandIn, CAP_GROUPS,
    CAP_PROPAGATION_RECEIPTS, CAP_READ_RECEIPTS, LEFT_OUT_FILE, LEFT_OUT_GONE, LEFT_OUT_PHOTO,
    LEFT_OUT_VOICE, MEMBER_ADMIN, MEMBER_JOINED, MEMBER_LEFT,
};
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;

use super::contacts::Ask;
use super::intent::Target;
use super::strings::{Arg, Lang, StrId, Text};
use super::{Message, Palette, UiEffect, UiModel};
use crate::when::WallTime;

/// The slot of [`UiModel::sending`] a group's state takes (a Join, a
/// Leave, a Remove): it runs in the shell's queue like every send, and no
/// bubble follows it, as a receipt's own.
pub const STATE_SEND: usize = usize::MAX - 1;

/// The capabilities every Loquax client says on every message it sends
/// (1307): bit 1, it understands groups, always; bit 2, it acknowledges
/// propagated post and wants its own acknowledged, always (1395, not
/// switchable); bit 0, read receipts, while the reader's switch is on.
pub fn client_caps(receipts: bool) -> u64 {
    CAP_GROUPS | CAP_PROPAGATION_RECEIPTS | if receipts { CAP_READ_RECEIPTS } else { 0 }
}

/// Why the home did not make a group (1307 pass five): the answer to
/// [`UiEffect::CreateGroup`] that is no address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotMade {
    /// The home answers, but not `/group`: it was built before groups,
    /// and no second ask changes that.
    HomeTooOld,
    /// Anything else, in the machinery's words; another ask may work.
    Failed(String),
}

/// Where the reader stands in a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// An invitation came; no Join yet.
    Invited,
    /// In it: joined, or its admin.
    Joined,
    /// Left, or removed.
    Left,
    /// Its admin deleted it (1307 pass four): the history stays readable,
    /// nothing can be written to it any more.
    Ended,
}

/// One member as the newest membership says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMember {
    pub address: [u8; 16],
    /// The name the admin gave it, the same for every member.
    pub name: String,
    pub admin: bool,
    pub joined: bool,
    pub left: bool,
}

/// A group the reader is in, was invited to or left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub address: [u8; 16],
    pub name: String,
    /// The admin's name as the invitation gave it; empty for the reader's
    /// own group.
    pub admin_name: String,
    /// Whose conversation the invitation is in.
    pub inviter: Option<[u8; 16]>,
    /// The reader made it: its admin.
    pub ours: bool,
    /// The newest membership's epoch, 0 before one came.
    pub epoch: u64,
    pub members: Vec<GroupMember>,
    pub standing: Standing,
}

impl GroupInfo {
    /// The members as the store keeps them (1307 G2): the `lev` key `0x05`
    /// as the wire carries it, the epoch with them.
    pub fn members_bytes(&self) -> Vec<u8> {
        let lev = Lev {
            members: Some(Members {
                epoch: self.epoch,
                members: self
                    .members
                    .iter()
                    .map(|m| Member {
                        address: m.address,
                        name: m.name.clone(),
                        flags: if m.left { MEMBER_LEFT } else { 0 }
                            | if m.admin { MEMBER_ADMIN } else { 0 }
                            | if m.joined { MEMBER_JOINED } else { 0 },
                    })
                    .collect(),
                ended: false,
            }),
            ..Lev::default()
        };
        let [_, (_, data)] = lev.fields();
        data
    }

    /// [`GroupInfo::members_bytes`] read back: the epoch and the members;
    /// `None` for bytes that are no membership.
    pub fn members_from(bytes: &[u8]) -> Option<(u64, Vec<GroupMember>)> {
        let [kind, _] = Lev::default().fields();
        let members = Lev::from_fields(&[kind, (FIELD_CUSTOM_DATA, bytes.to_vec())])?.members?;
        Some((
            members.epoch,
            members.members.iter().map(member_of).collect(),
        ))
    }

    /// The group at `address` with nothing known of it yet.
    pub fn new(address: [u8; 16]) -> Self {
        Self {
            address,
            name: String::new(),
            admin_name: String::new(),
            inviter: None,
            ours: false,
            epoch: 0,
            members: Vec::new(),
            standing: Standing::Invited,
        }
    }

    /// The members present now, the avatar's count; the reader and the
    /// admin among them.
    pub fn count(&self) -> usize {
        self.members.iter().filter(|m| !m.left).count()
    }

    /// The member at `address`, when the membership names it.
    pub fn member(&self, address: &[u8; 16]) -> Option<&GroupMember> {
        self.members.iter().find(|m| m.address == *address)
    }
}

/// The "New group" flow's step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Tick the friends.
    Pick,
    /// Type the name; the trust sentence under it; Create.
    Name,
}

/// The group being made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub step: Step,
    /// The friends ticked, in the order they were.
    pub picked: Vec<[u8; 16]>,
    pub name: String,
    /// Create was pressed; the home has not answered yet.
    pub asked: bool,
    /// Adding to this group rather than making one (1307 G2, study 4.5):
    /// the picker's Next is "Add", and there is no name step.
    pub to: Option<[u8; 16]>,
}

/// What the member sheet asks before it acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    Leave,
    Remove([u8; 16]),
    /// The admin's "Delete group" (1307 pass four).
    Delete,
}

/// The member sheet of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sheet {
    pub group: [u8; 16],
    pub confirm: Option<Confirm>,
}

/// The reader's verbs on groups: what a tap, a click or a palette command
/// asks ([`super::intent::Intent::Group`], [`super::UiEvent::Group`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verb {
    /// "New group" at the top of the new chat's list: the picker opens.
    New,
    /// A friend's tick, on or off.
    Pick([u8; 16]),
    /// The picker's Next: the name step.
    Next,
    /// What the name field holds now (the platform's text).
    Name(String),
    /// Create: the home is asked to make it.
    Create,
    /// "Join" on an invitation's card; the open one is the open group, or
    /// the newest invitation the open conversation holds.
    Join { group: Target },
    /// The group header: the member sheet opens.
    Members { group: Target },
    /// "Leave group": asked first unless `confirmed`.
    Leave { group: Target, confirmed: bool },
    /// The admin's "Add members" on the member sheet (1307 G2): the
    /// picker opens for the group; its Next adds the ticked friends.
    Add { group: Target },
    /// The admin's "Remove" on a member: asked first unless `confirmed`;
    /// with no member the palette asks for its name.
    Remove {
        group: Target,
        member: Option<[u8; 16]>,
        confirmed: bool,
    },
    /// The admin's "Delete group" on the member sheet (1307 pass four):
    /// asked first unless `confirmed`; the home deletes it for everyone.
    Delete { group: Target, confirmed: bool },
    /// "Start again with the same people" on a group the reader left or
    /// that ended (study 4.7): "New group" opens at its name step, the
    /// members ticked and the old name in the field; Create makes it.
    Again { group: Target },
    /// The picker, the sheet or its question closes.
    Close,
}

/// A member as a membership (`0x05`) lists it.
fn member_of(m: &Member) -> GroupMember {
    GroupMember {
        address: m.address,
        name: m.name.clone(),
        admin: m.flags & MEMBER_ADMIN != 0,
        joined: m.flags & (MEMBER_JOINED | MEMBER_ADMIN) != 0,
        left: m.flags & MEMBER_LEFT != 0,
    }
}

/// Whether a message of `body` and `fields` is only a group's state: no
/// words, no field but the `lev` pair, and a `lev` that is about a group
/// (a membership, or a Join's bare mark to a group).
pub fn is_state(body: &str, fields: &[Field]) -> bool {
    body.trim().is_empty()
        && !fields.is_empty()
        && fields
            .iter()
            .all(|(key, _)| *key == FIELD_CUSTOM_TYPE || *key == FIELD_CUSTOM_DATA)
        && Lev::from_fields(fields).is_some_and(|lev| {
            lev.receipt.is_none() && (lev.members.is_some() || lev.supports_groups())
        })
}

/// The writer of a group's copy and the text the home put after its
/// `"Name: "` prefix (study 3.1): `(writer, name, text)`. None for a
/// message that is no copy.
///
/// The writer comes from the copy's `0x02` and the name from `info`, the
/// group's membership, and exactly `"name: "` (or `"name:"`) is taken off,
/// so a name with a colon in it stays whole (1388). A copy whose text is
/// the home's sentence for a message without words, `"Photo from Anna"`
/// ([`StandIn`]), has no text: the bubble draws the photo under its
/// writer. Only for a writer the membership does not name is the name
/// what stands before the first colon, or after the sentence's words.
pub fn copy_of<'a>(
    body: &'a str,
    fields: &[Field],
    info: Option<&'a GroupInfo>,
) -> Option<([u8; 16], &'a str, &'a str)> {
    let copy = Lev::from_fields(fields)?.group_copy?;
    let known = info
        .and_then(|info| info.member(&copy.sender))
        .map(|m| m.name.as_str())
        .filter(|name| !name.is_empty());
    if let Some(name) = known {
        if StandIn::read(body, fields, name) {
            return Some((copy.sender, name, ""));
        }
        if let Some(rest) = body
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return Some((copy.sender, name, rest.strip_prefix(' ').unwrap_or(rest)));
        }
    }
    if let Some(name) = StandIn::name_in(body, fields) {
        return Some((copy.sender, name, ""));
    }
    let (name, text) = match body.split_once(':') {
        Some((name, rest)) => (name, rest.strip_prefix(' ').unwrap_or(rest)),
        None => ("", body),
    };
    Some((copy.sender, name, text))
}

/// The words the terminal draws for a group's copy whose text is the
/// home's sentence for a message without words (1388): `"Anna:"`, the
/// writer as every other copy names them there, over the media. None for
/// every other message, whose text the terminal draws as it came.
pub fn stand_in_line(body: &str, fields: &[Field]) -> Option<String> {
    Lev::from_fields(fields)?.group_copy?;
    StandIn::name_in(body, fields).map(|name| alloc::format!("{name}:"))
}

/// The line a membership from the group at `index` of `messages` makes
/// visible (1307 G2): who joined, left or was added since the membership
/// before it in the same conversation, `"Anna joined. Bob left."`, with
/// the names the admin gave (`own`, the reader, is "You"), in `lang`: the
/// reader's language, from the membership's data and never from the
/// text the home put beside it for a stock client (1348). None for a
/// message that is no membership from the group, for the first one loaded
/// (nothing to compare it with) and for one that changes nobody's
/// standing. Read from the messages alone, so every device and every
/// restart draws the same lines.
pub fn membership_note(
    messages: &[Message],
    index: usize,
    own: Option<[u8; 16]>,
    lang: Lang,
) -> Option<String> {
    let list = |m: &Message| {
        (!m.outbound)
            .then(|| Lev::from_fields(&m.fields)?.members)
            .flatten()
    };
    let now = list(messages.get(index)?)?;
    // The reader's own line has its own words ("Du bist beigetreten.").
    let said = |m: &Member, theirs: StrId, ours: StrId| match own == Some(m.address) {
        true => Text::key(ours).resolve(lang),
        false => Text::Key(theirs, alloc::vec![Arg::Str(m.name.clone())]).resolve(lang),
    };
    // The group's end (1307 pass four) is a line of its own, the first list
    // loaded or not.
    if now.ended {
        return Some(
            match now.members.iter().find(|m| m.flags & MEMBER_ADMIN != 0) {
                Some(admin) => said(admin, StrId::GroupNoteEnded, StrId::GroupNoteYouEnded),
                None => Text::key(StrId::GroupNoteAdminEnded).resolve(lang),
            },
        );
    }
    let before = messages[..index].iter().rev().find_map(list)?;
    if now.epoch <= before.epoch {
        return None;
    }
    let mut lines = Vec::new();
    for member in &now.members {
        let was = before.members.iter().find(|b| b.address == member.address);
        let left = member.flags & MEMBER_LEFT != 0;
        let joined = member.flags & (MEMBER_JOINED | MEMBER_ADMIN) != 0;
        let (theirs, ours) = match was {
            Some(was) if left && was.flags & MEMBER_LEFT == 0 => {
                (StrId::GroupNoteLeft, StrId::GroupNoteYouLeft)
            }
            Some(was) if !left && joined && was.flags & (MEMBER_JOINED | MEMBER_ADMIN) == 0 => {
                (StrId::GroupNoteJoined, StrId::GroupNoteYouJoined)
            }
            Some(was) if !left && was.flags & MEMBER_LEFT != 0 => {
                (StrId::GroupNoteAdded, StrId::GroupNoteYouAdded)
            }
            Some(_) => continue,
            None if left => continue,
            None if joined => (StrId::GroupNoteJoined, StrId::GroupNoteYouJoined),
            None => (StrId::GroupNoteAdded, StrId::GroupNoteYouAdded),
        };
        lines.push(said(member, theirs, ours));
    }
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// The home's English sentence on the wire for a digest line that stands
/// for what it left out (1348, `lev 0x06`'s fourth element): what a stock
/// client reads, and what a Loquax reader replaces with its own words
/// ([`digest_line`]). `None` for a value this build does not know.
pub fn left_out_wire(left_out: u64) -> Option<&'static str> {
    left_out_words(left_out).map(|id| id.text(Lang::En))
}

fn left_out_words(left_out: u64) -> Option<StrId> {
    match left_out {
        LEFT_OUT_PHOTO => Some(StrId::GroupLeftOutPhoto),
        LEFT_OUT_VOICE => Some(StrId::GroupLeftOutVoice),
        LEFT_OUT_FILE => Some(StrId::GroupLeftOutFile),
        LEFT_OUT_GONE => Some(StrId::GroupLeftOutGone),
        _ => None,
    }
}

/// A digest line's words as the reader sees them (1348): a line that
/// stands for what the home left out says so in `lang`, its caption after
/// it; every other line, and one whose value this build does not know, is
/// its text.
pub fn digest_line(line: &DigestLine, lang: Lang) -> String {
    let (Some(words), Some(wire)) = (
        line.left_out.and_then(left_out_words),
        line.left_out.and_then(left_out_wire),
    ) else {
        return line.text.clone();
    };
    let said = words.text(lang);
    match line
        .text
        .strip_prefix(wire)
        .and_then(|rest| rest.strip_prefix(": "))
    {
        Some(caption) if !caption.trim().is_empty() => alloc::format!("{said}: {caption}"),
        _ => said.to_string(),
    }
}

impl Message {
    /// Whether this message is a group's state and no bubble
    /// ([`is_state`]).
    pub fn is_group_state(&self) -> bool {
        is_state(&self.body, &self.fields)
    }

    /// Whether a screen draws this message as no block of its own: a
    /// reaction (drawn under its target) or a group's state.
    pub fn is_aside(&self) -> bool {
        self.is_reaction() || self.is_group_state()
    }
}

impl<B> UiModel<B> {
    /// The group at `peer`, when the reader is in it, was invited or left.
    pub fn group(&self, peer: &[u8; 16]) -> Option<&GroupInfo> {
        self.groups.get(peer)
    }

    /// The people a new conversation lists (1448, walk finding 17), as a
    /// messenger's contact list: every conversation with a person, the
    /// list's and the archived ones, by the name every screen gives them,
    /// ignoring case; no group and not the reader. Conversation indices.
    pub fn people(&self) -> Vec<usize> {
        let mut people: Vec<(String, usize)> = self
            .order()
            .into_iter()
            .chain(self.archived_order())
            .filter(|&c| {
                let peer = self.conversations[c].peer;
                !self.groups.contains_key(&peer) && self.address != Some(peer)
            })
            .map(|c| {
                let name = self.names.label(&self.conversations[c].peer, self.lang);
                (name.to_lowercase(), c)
            })
            .collect();
        people.sort();
        people.dedup_by_key(|(_, c)| *c);
        people.into_iter().map(|(_, c)| c).collect()
    }

    /// The friends the "New group" picker lists: every conversation the
    /// list shows that is no group, in the list's order.
    pub fn group_candidates(&self) -> Vec<[u8; 16]> {
        // Adding to a group (G2): its present members are no candidates.
        let adding = self
            .group_draft
            .as_ref()
            .and_then(|d| d.to)
            .and_then(|group| self.groups.get(&group));
        self.order()
            .into_iter()
            .map(|c| self.conversations[c].peer)
            .filter(|peer| !self.groups.contains_key(peer) && self.address != Some(*peer))
            .filter(|peer| adding.is_none_or(|group| group.member(peer).is_none_or(|m| m.left)))
            .collect()
    }

    /// Whether a person wrote in `peer`'s conversation (1307 pass six):
    /// `peer` is no group, and a message came from it that is no group's
    /// state and no invitation. The phone's "save in your contacts?"
    /// banner asks only then: a group is not a person, and an invitation
    /// card is the group's, not its sender's.
    pub fn heard_from_a_person(&self, peer: &[u8; 16]) -> bool {
        self.words_from_a_person(peer).next().is_some()
    }

    /// When the person `peer` first wrote, by the writer's clock (1448,
    /// walk finding 12): the earliest of the messages
    /// [`Self::heard_from_a_person`] counts that carries a time. The
    /// phone asks to save a contact only when this falls after the
    /// device began, so a history a new device pulls asks nothing.
    /// `None` when no such message carries a time.
    pub fn first_word_from_a_person(&self, peer: &[u8; 16]) -> Option<crate::when::WallTime> {
        self.words_from_a_person(peer)
            .filter_map(|m| m.at)
            .min_by_key(|at| at.unix_secs)
    }

    /// The messages a person wrote in `peer`'s conversation: none for a
    /// group, and neither a group's state nor an invitation card.
    fn words_from_a_person<'a>(
        &'a self,
        peer: &'a [u8; 16],
    ) -> impl Iterator<Item = &'a crate::ui::Message> + 'a {
        let person = !self.groups.contains_key(peer);
        self.conversations
            .iter()
            .filter(move |c| person && c.peer == *peer)
            .take(1)
            .flat_map(|c| c.messages.iter())
            .filter(|m| {
                !m.outbound
                    && !m.is_group_state()
                    && Lev::from_fields(&m.fields).is_none_or(|lev| lev.invite.is_none())
            })
    }

    /// Where the reader's invitation of `member` to `group` stands (1307
    /// pass six): the delivery of the newest invitation to it in the
    /// member's conversation, which the home's outbox reports through
    /// the archive as for any message sent. `None` without one, as on a
    /// device that invited nobody.
    pub fn invitation_delivery(
        &self,
        group: &[u8; 16],
        member: &[u8; 16],
    ) -> Option<leviculum_ui::delivery::DeliveryState> {
        self.conversations
            .iter()
            .find(|c| c.peer == *member)?
            .messages
            .iter()
            .rev()
            .find(|m| {
                m.outbound
                    && Lev::from_fields(&m.fields)
                        .and_then(|lev| lev.invite)
                        .is_some_and(|invite| invite.group == *group)
            })
            .map(|m| {
                m.delivery
                    .unwrap_or(leviculum_ui::delivery::DeliveryState::Queued)
            })
    }

    /// Learn what a message in `peer`'s conversation says of a group:
    /// `outbound` when the reader (or another of its devices) sent it. A
    /// group that changed is written to the store with the next writes
    /// (1307 G2), so a restart lists it as it stood.
    pub(super) fn learn_group(&mut self, peer: [u8; 16], outbound: bool, fields: &[Field]) {
        let Some(lev) = Lev::from_fields(fields) else {
            return;
        };
        let touched: Vec<[u8; 16]> = lev
            .invite
            .as_ref()
            .map(|invite| invite.group)
            .into_iter()
            .chain(core::iter::once(peer))
            .collect();
        let before: Vec<Option<GroupInfo>> = touched
            .iter()
            .map(|a| self.groups.get(a).cloned())
            .collect();
        self.learn_lev(peer, outbound, &lev);
        for (address, was) in touched.into_iter().zip(before) {
            if self.groups.get(&address) != was.as_ref() {
                self.groups_changed.insert(address);
            }
        }
    }

    fn learn_lev(&mut self, peer: [u8; 16], outbound: bool, lev: &Lev) {
        let own = self.address;
        if let (Some(status), false) = (&lev.status, outbound) {
            self.learn_status(peer, status);
        }
        if let Some(invite) = &lev.invite {
            let group = self
                .groups
                .entry(invite.group)
                .or_insert_with(|| GroupInfo::new(invite.group));
            group.name = invite.name.clone();
            if outbound {
                group.ours = true;
                group.standing = Standing::Joined;
            } else if !group.ours {
                group.admin_name = invite.admin_name.clone();
                group.inviter = Some(peer);
            }
            let name = invite.name.clone();
            self.names.remember(invite.group, &name);
            // The group has a row of its own from the invitation on: an
            // invitation before the Join, the group after it.
            if !self.conversations.iter().any(|c| c.peer == invite.group) {
                let index = self.conversation(invite.group);
                let activity = self.bump();
                self.conversations[index].activity = activity;
                self.conversations[index].touched = self.now.map(|now| now.unix_secs);
            }
        }
        if let Some(members) = &lev.members {
            if outbound {
                // Our own Leave, or the admin's Remove: the list the home
                // answers with follows; a Leave is taken at once.
                if let (Some(group), Some(own)) = (self.groups.get_mut(&peer), own) {
                    let [only] = members.members.as_slice() else {
                        return;
                    };
                    if only.address == own && only.flags & MEMBER_LEFT != 0 {
                        group.standing = Standing::Left;
                    }
                }
                return;
            }
            // Only a group the reader knows from an invitation, or made,
            // tells it members: anyone else's list is a stranger's word.
            let Some(group) = self.groups.get_mut(&peer) else {
                return;
            };
            if members.epoch >= group.epoch {
                group.epoch = members.epoch;
                group.members = members.members.iter().map(member_of).collect();
                if members.ended {
                    // Deleted by its admin (1307 pass four): nothing more
                    // is written to it; the admin's own devices forget it
                    // once the message is in ([`UiModel::forget_deleted_groups`]).
                    group.standing = Standing::Ended;
                } else if let Some(me) = own.and_then(|own| group.member(&own)).cloned() {
                    group.ours |= me.admin;
                    group.standing = match (me.left, me.joined) {
                        (true, _) => Standing::Left,
                        (false, true) => Standing::Joined,
                        (false, false) => group.standing,
                    };
                }
            }
        } else if outbound && lev.supports_groups() {
            // Our Join: an empty message with the mark, to a group we
            // were invited to.
            if let Some(group) = self.groups.get_mut(&peer) {
                if group.standing == Standing::Invited && lev.invite.is_none() {
                    group.standing = Standing::Joined;
                }
            }
        }
    }

    /// The newest status the group at `peer` sent of our message `id`
    /// (1307 pass four, `lev 0x03`): the detail view's "delivered to N of
    /// M" and "read by N of M".
    pub fn group_status(&self, peer: &[u8; 16], id: &[u8; 32]) -> Option<GroupStatus> {
        if !self.groups.contains_key(peer) {
            return None;
        }
        self.conversations
            .iter()
            .find(|c| c.peer == *peer)?
            .messages
            .iter()
            .rev()
            .filter(|m| !m.outbound)
            .filter_map(|m| Lev::from_fields(&m.fields)?.status)
            .find(|status| status.original == *id)
    }

    /// The detail view's lines of our message `id` in the group at `peer`
    /// (1307 pass four): how many have it, and how many of those that send
    /// read receipts read it. Empty where `peer` is no group.
    pub fn group_detail_rows(&self, peer: &[u8; 16], id: &[u8; 32]) -> Vec<String> {
        if !self.groups.contains_key(peer) {
            return Vec::new();
        }
        let Some(status) = self.group_status(peer, id) else {
            return alloc::vec!["group  no member's copy is confirmed yet".to_string()];
        };
        let mut rows = alloc::vec![alloc::format!(
            "group  delivered to {} of {}",
            status.delivered,
            status.members
        )];
        if let Some(on_device) = status.on_device {
            rows.push(alloc::format!(
                "       on the devices of {on_device} of {}",
                status.members
            ));
        }
        rows.push(match status.readers {
            0 => "read   nobody in it sends read receipts".to_string(),
            readers => alloc::format!("read   by {} of {readers}", status.read),
        });
        let quiet = status.members.saturating_sub(status.readers);
        if quiet > 0 && status.readers > 0 {
            rows.push(alloc::format!(
                "       {quiet} send no read receipts and are not counted"
            ));
        }
        for (name, rung) in self.group_rungs(peer, id) {
            let at = |t: u32| WallTime {
                unix_secs: i64::from(t),
                offset_secs: self.now.map_or(0, |n| n.offset_secs),
            };
            let said = match (rung.read_at, rung.on_device_at, rung.proven_at) {
                (r, _, _) if r > 0 => alloc::format!("read {}", at(r).hh_mm()),
                (_, d, _) if d > 0 => alloc::format!("on the device {}", at(d).hh_mm()),
                (_, _, p) if p > 0 => alloc::format!("arrived {}", at(p).hh_mm()),
                _ => "not arrived yet".to_string(),
            };
            rows.push(alloc::format!("       {name}: {said}"));
        }
        rows
    }

    /// Each member's rungs of our message `id` in the group at `peer`, as
    /// the newest status says them (1466), with the name the group gave the
    /// member: the Info's row per member. Empty where `peer` is no group or
    /// its home said no rungs (before 1466).
    pub fn group_rungs(&self, peer: &[u8; 16], id: &[u8; 32]) -> Vec<(String, MemberRung)> {
        let Some(group) = self.groups.get(peer) else {
            return Vec::new();
        };
        let Some(status) = self.group_status(peer, id) else {
            return Vec::new();
        };
        status
            .rungs
            .into_iter()
            .map(|rung| {
                let name = group
                    .member(&rung.member)
                    .map(|m| m.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| self.names.label(&rung.member, self.lang));
                (name, rung)
            })
            .collect()
    }

    /// The group's status of our message (1307 pass four, 1466): one tick,
    /// arrived, once every present member's copy is proven; two once every
    /// copy is on its member's device; read once every member that sends
    /// receipts read it. The counts go into its ledger either way, so the
    /// detail view says them. No rung is taken back ([`status_rung`]).
    fn learn_status(&mut self, peer: [u8; 16], status: &GroupStatus) {
        if !self.groups.contains_key(&peer) {
            return;
        }
        let Some(c) = self.conversations.iter().position(|c| c.peer == peer) else {
            return;
        };
        let Some(m) = self.conversations[c]
            .messages
            .iter()
            .position(|m| m.outbound && m.id == Some(status.original))
        else {
            return;
        };
        let now = self.now;
        let at = now.map_or(0, |n| u32::try_from(n.unix_secs).unwrap_or(0));
        let next = status_rung(status, at);
        let Some(mut message) = self.message_mut(c, m) else {
            return;
        };
        message.note(now, &status_line(status));
        if let Some(next) = next {
            if rank(Some(next)) > rank(message.delivery) {
                message.delivery = Some(next);
            }
        }
    }

    /// Forget every group its admin deleted that the reader made (1307
    /// pass four): the admin's devices keep no copy, the conversation and
    /// its messages go. Called once the message that said so is in.
    pub(super) fn forget_deleted_groups(&mut self) {
        let gone: Vec<[u8; 16]> = self
            .groups
            .values()
            .filter(|g| g.ours && g.standing == Standing::Ended)
            .map(|g| g.address)
            .collect();
        for group in gone {
            self.forget_group(group);
        }
    }

    /// The group at `group` and its conversation leave this device.
    fn forget_group(&mut self, group: [u8; 16]) {
        self.groups.remove(&group);
        self.groups_changed.remove(&group);
        self.owed_groups.remove(&group);
        if self.group_sheet.is_some_and(|s| s.group == group) {
            self.group_sheet = None;
        }
        self.delete_conversation(group, false);
    }

    /// Learn every group the restored conversations speak of.
    pub(super) fn learn_groups(&mut self) {
        let seen: Vec<([u8; 16], bool, Vec<Field>)> = self
            .conversations
            .iter()
            .flat_map(|c| {
                c.messages
                    .iter()
                    .filter(|m| Lev::from_fields(&m.fields).is_some())
                    .map(|m| (c.peer, m.outbound, m.fields.clone()))
            })
            .collect();
        for (peer, outbound, fields) in seen {
            self.learn_group(peer, outbound, &fields);
        }
        self.forget_deleted_groups();
    }
}

/// The rung a group's status puts our line on (1466): read once every
/// member that sends receipts read it, two ticks once every copy is on its
/// member's device, one tick, arrived, once every copy is proven; none
/// before. `at` is this device's clock, the read rung's time.
pub fn status_rung(status: &GroupStatus, at: u32) -> Option<DeliveryState> {
    if status.read_by_all() {
        Some(DeliveryState::Read { at })
    } else if status.on_devices_of_all() {
        Some(DeliveryState::Delivered)
    } else if status.delivered_to_all() {
        Some(DeliveryState::Arrived)
    } else {
        None
    }
}

/// Where a state stands on the ladder a group's line climbs: nothing a
/// status says moves it down, and the group's proof (in the mailbox) is
/// the lowest of the rungs above sending.
fn rank(state: Option<DeliveryState>) -> u8 {
    match state {
        Some(DeliveryState::Read { .. }) => 4,
        Some(DeliveryState::Delivered) => 3,
        Some(DeliveryState::Arrived) => 2,
        Some(DeliveryState::InMailbox) => 1,
        _ => 0,
    }
}

/// A line of ours kept from before 1466 at two ticks in a group: its two
/// came from a status that counted proofs, which a member's mailbox signs
/// too. Unless a status it kept says every copy is on a device, it shows
/// one tick now. The store applies it as it loads a message, beside
/// [`super::arrival::before_1462`]; the next write keeps it.
pub fn before_1466(message: &mut Message) {
    if !message.outbound || message.delivery != Some(DeliveryState::Delivered) {
        return;
    }
    let statuses: Vec<Vec<u64>> = message
        .ledger
        .iter()
        .filter_map(|entry| {
            let rest = entry
                .what
                .strip_prefix(STATUS_LINE)
                .or(entry.what.strip_prefix(OLD_STATUS_LINE))?;
            Some(
                rest.split(|c: char| !c.is_ascii_digit())
                    .filter(|n| !n.is_empty())
                    .filter_map(|n| n.parse().ok())
                    .collect(),
            )
        })
        .collect();
    if statuses.is_empty() {
        return;
    }
    // `group-status delivered members read readers on_device`.
    let on_every_device = statuses
        .iter()
        .any(|n| matches!(n.as_slice(), &[_, members, _, _, on_device] if members > 0 && on_device >= members));
    if !on_every_device {
        message.delivery = Some(DeliveryState::Arrived);
    }
}

/// The start of a group status's ledger line (1381): the four numbers
/// follow it, so the line is data and [`ledger_line`] says it in the
/// reader's language.
const STATUS_LINE: &str = "group-status";

/// The ledger line of `status`: `group-status 2 3 1 2`, delivered, members,
/// read and readers, and since 1466 the copies on a device after them,
/// `group-status 3 3 1 2 2`, when the home said it.
fn status_line(status: &GroupStatus) -> String {
    let mut line = alloc::format!(
        "{STATUS_LINE} {} {} {} {}",
        status.delivered,
        status.members,
        status.read,
        status.readers
    );
    if let Some(on_device) = status.on_device {
        line.push_str(&alloc::format!(" {on_device}"));
    }
    line
}

/// The English sentence a store written before 1381 kept for a status.
const OLD_STATUS_LINE: &str = "the group: delivered to ";

/// A ledger line in `lang`: a group status's numbers as the catalogue's
/// sentence, the English sentence an older store kept too; any other line
/// as it was written.
pub fn ledger_line(what: &str, lang: Lang) -> String {
    let numbers = |rest: &str| -> Vec<i64> {
        rest.split(|c: char| !c.is_ascii_digit())
            .filter(|n| !n.is_empty())
            .filter_map(|n| n.parse().ok())
            .collect()
    };
    let said = match (
        what.strip_prefix(STATUS_LINE),
        what.strip_prefix(OLD_STATUS_LINE),
    ) {
        (Some(rest), _) => Some(numbers(rest)),
        (None, Some(rest)) => Some(numbers(rest)).filter(|n| {
            n.len() == 4
                && what
                    == alloc::format!(
                        "{OLD_STATUS_LINE}{} of {}, read by {} of {}",
                        n[0],
                        n[1],
                        n[2],
                        n[3]
                    )
        }),
        (None, None) => None,
    };
    match said.as_deref() {
        Some(&[delivered, members, read, readers]) => Text::Key(
            StrId::GroupStatusNote,
            alloc::vec![
                Arg::Int(delivered),
                Arg::Int(members),
                Arg::Int(read),
                Arg::Int(readers),
            ],
        )
        .resolve(lang),
        Some(&[delivered, members, read, readers, on_device]) => Text::Key(
            StrId::GroupStatusDevicesNote,
            alloc::vec![
                Arg::Int(delivered),
                Arg::Int(members),
                Arg::Int(on_device),
                Arg::Int(read),
                Arg::Int(readers),
            ],
        )
        .resolve(lang),
        _ => what.to_string(),
    }
}

impl<B: Default> UiModel<B> {
    /// The group a verb names: the open conversation's when it is a group,
    /// else for a Join the newest invitation the open conversation holds
    /// that waits for one.
    fn group_target(&self, target: Target, inviting: bool) -> Option<[u8; 16]> {
        let peer = match target {
            Target::Peer(peer) => return Some(peer),
            Target::Open => self.conversations.get(self.selected)?.peer,
        };
        if self.groups.contains_key(&peer) {
            return Some(peer);
        }
        if !inviting {
            return None;
        }
        self.conversations
            .get(self.selected)?
            .messages
            .iter()
            .rev()
            .filter_map(|m| Lev::from_fields(&m.fields)?.invite)
            .map(|invite| invite.group)
            .find(|group| {
                self.groups
                    .get(group)
                    .is_some_and(|g| g.standing == Standing::Invited)
            })
    }

    /// One of the reader's verbs on groups.
    pub(super) fn group_verb(&mut self, verb: Verb) -> Vec<UiEffect> {
        match verb {
            Verb::New => {
                self.group_draft = Some(Draft {
                    step: Step::Pick,
                    picked: Vec::new(),
                    name: String::new(),
                    asked: false,
                    to: None,
                });
                self.group_sheet = None;
            }
            Verb::Add { group } => {
                let Some(group) = self.group_target(group, false) else {
                    return Vec::new();
                };
                if !self.groups.get(&group).is_some_and(|g| g.ours) {
                    self.toast(
                        leviculum_ui::toast::ToastKind::Info,
                        "only the group's admin adds members".to_string(),
                    );
                    return Vec::new();
                }
                self.group_draft = Some(Draft {
                    step: Step::Pick,
                    picked: Vec::new(),
                    name: String::new(),
                    asked: false,
                    to: Some(group),
                });
                self.group_sheet = None;
            }
            Verb::Pick(peer) => {
                if let Some(draft) = self.group_draft.as_mut() {
                    match draft.picked.iter().position(|p| *p == peer) {
                        Some(at) => {
                            draft.picked.remove(at);
                        }
                        None if self.groups.contains_key(&peer) => {}
                        None => draft.picked.push(peer),
                    }
                }
            }
            Verb::Next => {
                if let Some(draft) = self.group_draft.as_mut() {
                    if draft.to.is_some() {
                        return self.add_members();
                    }
                    if !draft.picked.is_empty() {
                        draft.step = Step::Name;
                    }
                }
            }
            Verb::Name(name) => {
                if let Some(draft) = self.group_draft.as_mut() {
                    draft.name = name;
                }
            }
            Verb::Create => return self.create_group(),
            Verb::Join { group } => {
                return match self.group_target(group, true) {
                    Some(group) => self.join_group(group),
                    None => {
                        self.toast(
                            leviculum_ui::toast::ToastKind::Info,
                            "no invitation here to join".to_string(),
                        );
                        Vec::new()
                    }
                }
            }
            Verb::Members { group } => {
                if let Some(group) = self.group_target(group, false) {
                    self.group_sheet = Some(Sheet {
                        group,
                        confirm: None,
                    });
                }
            }
            Verb::Leave { group, confirmed } => {
                let Some(group) = self.group_target(group, false) else {
                    return Vec::new();
                };
                let ended = self
                    .groups
                    .get(&group)
                    .is_some_and(|g| g.standing == Standing::Ended);
                if ended && confirmed {
                    // "Remove from list": an ended group has nobody to
                    // tell; it leaves this device (1307 pass four).
                    self.forget_group(group);
                    return Vec::new();
                }
                if !confirmed {
                    self.group_sheet = Some(Sheet {
                        group,
                        confirm: Some(Confirm::Leave),
                    });
                    return Vec::new();
                }
                self.group_sheet = None;
                let Some(own) = self.address else {
                    return Vec::new();
                };
                return self.group_state(group, Some((own, MEMBER_LEFT)));
            }
            Verb::Remove {
                group,
                member,
                confirmed,
            } => {
                let Some(group) = self.group_target(group, false) else {
                    return Vec::new();
                };
                if !self.groups.get(&group).is_some_and(|g| g.ours) {
                    self.toast(
                        leviculum_ui::toast::ToastKind::Info,
                        "only the group's admin removes members".to_string(),
                    );
                    return Vec::new();
                }
                let Some(member) = member else {
                    self.palette = Some(Palette {
                        ask: Some(Ask::RemoveMember(group)),
                        ..Palette::default()
                    });
                    return Vec::new();
                };
                if !confirmed {
                    self.group_sheet = Some(Sheet {
                        group,
                        confirm: Some(Confirm::Remove(member)),
                    });
                    return Vec::new();
                }
                self.group_sheet = Some(Sheet {
                    group,
                    confirm: None,
                });
                return self.group_state(group, Some((member, MEMBER_LEFT)));
            }
            Verb::Delete { group, confirmed } => {
                let Some(group) = self.group_target(group, false) else {
                    return Vec::new();
                };
                let admin = self
                    .groups
                    .get(&group)
                    .is_some_and(|g| g.ours && g.standing == Standing::Joined);
                if !admin {
                    self.toast(
                        leviculum_ui::toast::ToastKind::Info,
                        "only the group's admin deletes it".to_string(),
                    );
                    return Vec::new();
                }
                if !confirmed {
                    self.group_sheet = Some(Sheet {
                        group,
                        confirm: Some(Confirm::Delete),
                    });
                    return Vec::new();
                }
                self.group_sheet = Some(Sheet {
                    group,
                    confirm: None,
                });
                return alloc::vec![UiEffect::DeleteGroup { group }];
            }
            Verb::Again { group } => {
                let Some(group) = self.group_target(group, false) else {
                    return Vec::new();
                };
                let Some(info) = self
                    .groups
                    .get(&group)
                    .filter(|g| matches!(g.standing, Standing::Left | Standing::Ended))
                else {
                    return Vec::new();
                };
                let picked: Vec<[u8; 16]> = info
                    .members
                    .iter()
                    .map(|m| m.address)
                    .filter(|a| self.address != Some(*a))
                    .collect();
                if picked.is_empty() {
                    return Vec::new();
                }
                self.group_draft = Some(Draft {
                    step: Step::Name,
                    picked,
                    name: info.name.clone(),
                    asked: false,
                    to: None,
                });
                self.group_sheet = None;
            }
            Verb::Close => match self.group_sheet.as_mut() {
                Some(sheet) if sheet.confirm.is_some() => sheet.confirm = None,
                Some(_) => self.group_sheet = None,
                None => self.group_draft = None,
            },
        }
        Vec::new()
    }
}

impl<B> UiModel<B> {
    /// The palette's rows while it asks for a group (the terminal's
    /// picker, name, members and questions).
    pub fn group_ask_rows(&self, ask: Ask) -> Vec<String> {
        let names = |peers: &[[u8; 16]]| {
            peers
                .iter()
                .map(|p| self.names.label(p, self.lang))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match ask {
            Ask::GroupPick => {
                let picked = self
                    .group_draft
                    .as_ref()
                    .map(|d| names(&d.picked))
                    .unwrap_or_default();
                let mut rows = alloc::vec![
                    super::strings::StrId::AskGroupTick
                        .text(self.lang)
                        .to_string(),
                    "Enter on an empty line goes on to the name".to_string(),
                ];
                if !picked.is_empty() {
                    rows.push(alloc::format!("ticked: {picked}"));
                }
                rows
            }
            Ask::GroupName => alloc::vec![
                "the group's name".to_string(),
                "It is kept on your mailbox. Messages in the group can be read there.".to_string(),
            ],
            Ask::Members(group) => {
                let Some(info) = self.groups.get(&group) else {
                    return Vec::new();
                };
                let mut rows =
                    alloc::vec![alloc::format!("{} · {} members", info.name, info.count())];
                rows.extend(info.members.iter().map(|m| {
                    let words = member_words(m, self.address == Some(m.address));
                    alloc::format!("{} · {}", self.member_name(m), words)
                }));
                rows
            }
            Ask::LeaveGroup(group) => alloc::vec![alloc::format!(
                "leave {}? Enter leaves, Esc stays",
                self.names.label(&group, self.lang)
            ),],
            Ask::DeleteGroup(group) => alloc::vec![alloc::format!(
                "delete {} for everyone? Enter deletes, Esc keeps it",
                self.names.label(&group, self.lang)
            ),],
            Ask::RemoveMember(group) => alloc::vec![alloc::format!(
                "remove whom from {}? type a name, Enter",
                self.names.label(&group, self.lang)
            ),],
            _ => Vec::new(),
        }
    }

    /// A member's name as the reader sees it: ours, the reader's own word
    /// for the address, or the admin's.
    pub fn member_name(&self, member: &GroupMember) -> String {
        if self.address == Some(member.address) {
            return "You".to_string();
        }
        match self.names.shown(&member.address) {
            Some(shown) => shown.to_string(),
            None if !member.name.is_empty() => member.name.clone(),
            None => self.names.label(&member.address, self.lang),
        }
    }
}

impl<B: Default> UiModel<B> {
    /// `Enter` in a palette asking for a group.
    pub(super) fn answer_group_ask(&mut self, ask: Ask, typed: String) -> Vec<UiEffect> {
        let typed = typed.trim().to_string();
        match ask {
            Ask::GroupPick => {
                if typed.is_empty() {
                    let picked = self
                        .group_draft
                        .as_ref()
                        .is_some_and(|d| !d.picked.is_empty());
                    let adding = self.group_draft.as_ref().is_some_and(|d| d.to.is_some());
                    if picked && adding {
                        return self.group_verb(Verb::Next);
                    }
                    if picked {
                        self.group_verb(Verb::Next);
                        self.palette = Some(Palette {
                            ask: Some(Ask::GroupName),
                            ..Palette::default()
                        });
                    } else {
                        self.group_draft = None;
                    }
                    return Vec::new();
                }
                // A friend by name, or a pasted code or address (the
                // order's "members from contacts or pasted codes").
                let found = self.find_candidate(&typed).or_else(|| {
                    crate::code::read(&typed)
                        .ok()
                        .and_then(|code| code.address())
                        .filter(|peer| self.address != Some(*peer))
                });
                match found {
                    Some(peer) => {
                        self.group_verb(Verb::Pick(peer));
                    }
                    None => self.toast(
                        leviculum_ui::toast::ToastKind::Info,
                        super::strings::Text::Key(
                            super::strings::StrId::NoContactCalled,
                            alloc::vec![super::strings::Arg::Str(typed.to_string())],
                        )
                        .resolve(self.lang),
                    ),
                }
                self.palette = Some(Palette {
                    ask: Some(Ask::GroupPick),
                    ..Palette::default()
                });
                Vec::new()
            }
            Ask::GroupName => {
                // An empty Enter keeps the name the draft has: a group
                // started again keeps its old one (1307 pass four).
                if !typed.is_empty() {
                    self.group_verb(Verb::Name(typed));
                }
                self.group_verb(Verb::Create)
            }
            Ask::DeleteGroup(group) => self.group_verb(Verb::Delete {
                group: Target::Peer(group),
                confirmed: true,
            }),
            Ask::Members(_) => Vec::new(),
            Ask::LeaveGroup(group) => self.group_verb(Verb::Leave {
                group: Target::Peer(group),
                confirmed: true,
            }),
            Ask::RemoveMember(group) => {
                let lower = typed.to_lowercase();
                let member = self.groups.get(&group).and_then(|info| {
                    info.members
                        .iter()
                        .filter(|m| !m.left && self.address != Some(m.address))
                        .find(|m| self.member_name(m).to_lowercase().contains(&lower))
                        .map(|m| m.address)
                });
                match member {
                    Some(member) => self.group_verb(Verb::Remove {
                        group: Target::Peer(group),
                        member: Some(member),
                        confirmed: true,
                    }),
                    None => {
                        self.toast(
                            leviculum_ui::toast::ToastKind::Info,
                            alloc::format!("no member called {typed}"),
                        );
                        Vec::new()
                    }
                }
            }
            _ => Vec::new(),
        }
    }

    /// The candidate whose name holds `typed`, case folded: the first in
    /// the list's order.
    fn find_candidate(&self, typed: &str) -> Option<[u8; 16]> {
        let lower = typed.to_lowercase();
        self.group_candidates().into_iter().find(|peer| {
            self.names
                .label(peer, self.lang)
                .to_lowercase()
                .contains(&lower)
        })
    }

    /// Create: the home is asked, once, with the ticked friends under the
    /// names the reader knows them by.
    fn create_group(&mut self) -> Vec<UiEffect> {
        let Some(draft) = self.group_draft.as_mut() else {
            return Vec::new();
        };
        let name = draft.name.trim().to_string();
        if draft.asked || draft.step != Step::Name || name.is_empty() || draft.picked.is_empty() {
            return Vec::new();
        }
        draft.asked = true;
        let picked = draft.picked.clone();
        let members = picked
            .iter()
            .map(|peer| {
                let name = self
                    .names
                    .shown(peer)
                    .map_or_else(|| super::short(peer), ToString::to_string);
                (*peer, name)
            })
            .collect();
        alloc::vec![UiEffect::CreateGroup { name, members }]
    }

    /// The picker's "Add" (1307 G2): the home is asked once to add the
    /// ticked friends under the names the reader knows them by.
    fn add_members(&mut self) -> Vec<UiEffect> {
        let Some(draft) = self.group_draft.as_mut() else {
            return Vec::new();
        };
        let Some(group) = draft.to else {
            return Vec::new();
        };
        if draft.asked || draft.picked.is_empty() {
            return Vec::new();
        }
        draft.asked = true;
        let picked = draft.picked.clone();
        let members = picked
            .iter()
            .map(|peer| {
                let name = self
                    .names
                    .shown(peer)
                    .map_or_else(|| super::short(peer), ToString::to_string);
                (*peer, name)
            })
            .collect();
        alloc::vec![UiEffect::AddMembers { group, members }]
    }

    /// The home's answer to "Add" (G2): the picker closes and the member
    /// sheet opens on the group, whose new list follows from the home; or
    /// the reader is told why not and the picker stays.
    pub(super) fn members_added(&mut self, added: Result<[u8; 16], String>) -> Vec<UiEffect> {
        let Some(draft) = self.group_draft.take() else {
            return Vec::new();
        };
        match added {
            Ok(group) => {
                let count = draft.picked.len();
                self.group_sheet = Some(Sheet {
                    group,
                    confirm: None,
                });
                self.toast(
                    leviculum_ui::toast::ToastKind::Info,
                    alloc::format!("{count} invited"),
                );
            }
            Err(why) => {
                self.toast(
                    leviculum_ui::toast::ToastKind::Error,
                    alloc::format!("Nobody was added: {why}"),
                );
                self.group_draft = Some(Draft {
                    asked: false,
                    ..draft
                });
            }
        }
        Vec::new()
    }

    /// The home's answer to Create: the group's conversation is listed
    /// under its name, on top, and opened, and the group is written so a
    /// restart lists it before any message came (1307 pass five); or the
    /// reader is told why not, and nothing is listed.
    pub(super) fn group_made(&mut self, made: Result<[u8; 16], NotMade>) -> Vec<UiEffect> {
        let Some(draft) = self.group_draft.take() else {
            return Vec::new();
        };
        let address = match made {
            Ok(address) => address,
            // Asking again cannot help: the draft goes, and the one
            // sentence says what can.
            Err(NotMade::HomeTooOld) => {
                self.toast(
                    leviculum_ui::toast::ToastKind::Error,
                    super::strings::StrId::GroupHomeTooOld
                        .text(self.lang)
                        .to_string(),
                );
                return Vec::new();
            }
            Err(NotMade::Failed(why)) => {
                self.toast(
                    leviculum_ui::toast::ToastKind::Error,
                    alloc::format!("The group was not made: {why}"),
                );
                self.group_draft = Some(Draft {
                    asked: false,
                    ..draft
                });
                return Vec::new();
            }
        };
        let name = draft.name.trim().to_string();
        let own = self.address;
        let group = self
            .groups
            .entry(address)
            .or_insert_with(|| GroupInfo::new(address));
        group.name = name.clone();
        group.ours = true;
        group.standing = Standing::Joined;
        if group.members.is_empty() {
            group.members = own
                .into_iter()
                .map(|own| GroupMember {
                    address: own,
                    name: String::new(),
                    admin: true,
                    joined: true,
                    left: false,
                })
                .chain(draft.picked.iter().map(|peer| GroupMember {
                    address: *peer,
                    // The member's name as the reader shows it, no tail and
                    // no "Unknown": it travels in the group's state (1452).
                    name: self.names.shown(peer).unwrap_or_default().to_string(),
                    admin: false,
                    joined: false,
                    left: false,
                }))
                .collect();
        }
        self.names.remember(address, &name);
        self.groups_changed.insert(address);
        let index = self.conversation(address);
        let activity = self.bump();
        self.conversations[index].activity = activity;
        self.conversations[index].touched = self.now.map(|now| now.unix_secs);
        self.select(index);
        Vec::new()
    }

    /// The home's answer to "Delete group" (1307 pass four): the group and
    /// its conversation leave this device at once (the home's last
    /// membership tells the reader's other devices), or the reader is told
    /// why not and the sheet stays.
    pub(super) fn group_deleted(&mut self, deleted: Result<[u8; 16], String>) -> Vec<UiEffect> {
        match deleted {
            Ok(group) => {
                let name = self.names.label(&group, self.lang);
                self.forget_group(group);
                self.toast(
                    leviculum_ui::toast::ToastKind::Info,
                    alloc::format!("{name} deleted for everyone"),
                );
            }
            Err(why) => self.toast(
                leviculum_ui::toast::ToastKind::Error,
                alloc::format!("The group was not deleted: {why}"),
            ),
        }
        Vec::new()
    }

    /// Join: the group hears an empty message with the mark, and the home
    /// answers with the members.
    fn join_group(&mut self, group: [u8; 16]) -> Vec<UiEffect> {
        let Some(info) = self.groups.get(&group) else {
            return Vec::new();
        };
        if info.standing != Standing::Invited {
            return Vec::new();
        }
        let effects = self.group_state(group, None);
        if let Some(info) = self.groups.get_mut(&group) {
            info.standing = Standing::Joined;
        }
        let index = self.conversation(group);
        self.select(index);
        effects
    }

    /// One state message to `group`, no bubble: the mark alone (a Join), or
    /// the membership naming one member with `flags` (a Leave, a Remove).
    fn group_state(&mut self, group: [u8; 16], member: Option<([u8; 16], u64)>) -> Vec<UiEffect> {
        let Some(info) = self.groups.get(&group) else {
            return Vec::new();
        };
        let members = member.map(|(address, flags)| Members {
            epoch: info.epoch,
            members: alloc::vec![Member {
                address,
                name: info
                    .member(&address)
                    .map(|m| m.name.clone())
                    .unwrap_or_default(),
                flags,
            }],
            ended: false,
        });
        if let (Some(own), Some(Members { members, .. })) = (self.address, &members) {
            if members[0].address == own {
                if let Some(info) = self.groups.get_mut(&group) {
                    info.standing = Standing::Left;
                }
            }
        }
        let mut fields = Vec::new();
        Lev {
            capabilities: Some(client_caps(self.reading.on)),
            members,
            ..Lev::default()
        }
        .put_into(&mut fields);
        self.sending.push_back((STATE_SEND, 0));
        alloc::vec![UiEffect::Send {
            to: group,
            body: String::new(),
            fields,
        }]
    }
}

/// A member's state in words, as the sheet shows it under the name.
pub fn member_words(member: &GroupMember, reader: bool) -> &'static str {
    match (member.left, member.admin, member.joined || reader) {
        (true, _, _) => "left",
        (false, true, _) => "admin",
        (false, false, true) => "joined",
        (false, false, false) => "invited",
    }
}

/// Every group's [`GroupInfo`], by its address.
pub type Groups = BTreeMap<[u8; 16], GroupInfo>;

#[cfg(test)]
mod tests {
    use alloc::vec;

    use leviculum_lxmf::lev::{GroupCopy, Invite};

    use super::super::intent::Intent;
    use super::super::strings::Lang;
    use super::super::strings::StrId;
    use super::super::tests::Field as Line;
    use super::super::{update, UiEvent, NONE_OPEN};
    use super::*;
    use crate::outbox::OutboxEvent;

    const ME: [u8; 16] = [0x01; 16];
    const LEW: [u8; 16] = [0x02; 16];
    const ANNA: [u8; 16] = [0x03; 16];
    const BOB: [u8; 16] = [0x04; 16];
    const GROUP: [u8; 16] = [0x0f; 16];

    fn reader(peers: &[[u8; 16]]) -> UiModel<Line> {
        let mut model = UiModel::<Line>::new(peers);
        model.address = Some(ME);
        model
    }

    fn lev_fields(lev: Lev) -> Vec<leviculum_lxmf::Field> {
        let mut fields = Vec::new();
        lev.put_into(&mut fields);
        fields
    }

    fn arrive(model: &mut UiModel<Line>, from: [u8; 16], id: u8, body: &str, lev: Lev) {
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: lev_fields(lev),
                verified: true,
            }),
        );
    }

    fn invite(admin_name: &str) -> Lev {
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            invite: Some(Invite {
                group: GROUP,
                public_key: [7; 64],
                name: "Familie".into(),
                admin_name: admin_name.into(),
                epoch: 1,
            }),
            ..Lev::default()
        }
    }

    fn members(epoch: u64, list: &[([u8; 16], &str, u64)]) -> Lev {
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            members: Some(Members {
                epoch,
                members: list
                    .iter()
                    .map(|(address, name, flags)| Member {
                        address: *address,
                        name: (*name).into(),
                        flags: *flags,
                    })
                    .collect(),
                ended: false,
            }),
            ..Lev::default()
        }
    }

    fn copy(writer: [u8; 16]) -> Lev {
        Lev {
            capabilities: Some(CAP_GROUPS),
            group_copy: Some(GroupCopy {
                sender: writer,
                sent_ms: 1,
                epoch: 1,
            }),
            ..Lev::default()
        }
    }

    fn verb(model: &mut UiModel<Line>, verb: Verb) -> Vec<UiEffect> {
        update(model, UiEvent::Group(verb))
    }

    /// The study's create flow (4.1), counted: the new-chat button, "New
    /// group", a tick per friend, Next, the typed name and Create, 4 + k
    /// taps. The home is asked once with the friends under the reader's
    /// names; its answer lists and opens the group. Controls: Next with
    /// nobody ticked stays, Create without a name asks nothing, a second
    /// Create while the home has not answered asks nothing, a refusal keeps
    /// the draft and says why.
    #[test]
    fn a_group_is_made_in_four_taps_and_one_per_friend() {
        let mut model = reader(&[ANNA, BOB]);
        model.names.remember(ANNA, "Anna");
        model.names.remember(BOB, "Bob");
        let mut taps = 1; // the new-chat button, a frontend's
        assert!(verb(&mut model, Verb::New).is_empty());
        taps += 1;
        assert_eq!(model.group_candidates(), [ANNA, BOB]);
        verb(&mut model, Verb::Next);
        assert_eq!(model.group_draft.as_ref().map(|d| d.step), Some(Step::Pick));
        for friend in [ANNA, BOB] {
            verb(&mut model, Verb::Pick(friend));
            taps += 1;
        }
        verb(&mut model, Verb::Next);
        taps += 1;
        assert_eq!(model.group_draft.as_ref().map(|d| d.step), Some(Step::Name));
        assert!(verb(&mut model, Verb::Create).is_empty(), "no name yet");
        verb(&mut model, Verb::Name("Familie ".into()));
        let effects = verb(&mut model, Verb::Create);
        taps += 1;
        assert_eq!(taps, 4 + 2, "4 + k with k = 2");
        let [UiEffect::CreateGroup { name, members, .. }] = effects.as_slice() else {
            panic!("one ask: {effects:?}");
        };
        assert_eq!(name, "Familie");
        assert_eq!(
            members
                .iter()
                .map(|(a, n)| (*a, n.as_str()))
                .collect::<Vec<_>>(),
            [(ANNA, "Anna"), (BOB, "Bob")]
        );
        assert!(verb(&mut model, Verb::Create).is_empty(), "asked once");

        update(
            &mut model,
            UiEvent::GroupMade(Err(NotMade::Failed("no mailbox".into()))),
        );
        assert!(model.group_draft.is_some(), "kept for another try");
        assert!(model.current_toast().is_some());
        assert_eq!(verb(&mut model, Verb::Create).len(), 1, "asked again");
        update(&mut model, UiEvent::GroupMade(Ok(GROUP)));
        assert!(model.group_draft.is_none());
        let open = &model.conversations[model.selected];
        assert_eq!(open.peer, GROUP);
        assert_eq!(
            model.names.label(&GROUP, model.lang).split(' ').next(),
            Some("Familie")
        );
        let info = model.group(&GROUP).expect("the group");
        assert!(info.ours);
        assert_eq!(info.standing, Standing::Joined);
        assert_eq!(info.count(), 3, "the reader and two friends");
        assert!(
            !model.group_candidates().contains(&GROUP),
            "no group in a group"
        );
    }

    /// 1307 pass five, Lew's phone on a home built before groups: the
    /// home's silence on `/group` comes back as [`NotMade::HomeTooOld`],
    /// which says the one sentence in the reader's language and lists
    /// nothing, keeps no draft to ask again with and writes no group.
    /// Control: a confirming home lists the group at once, on top of the
    /// older chats, with no message in it, and writes it for a restart.
    #[test]
    fn a_home_too_old_for_groups_says_so_and_lists_nothing() {
        let asked = |lang| {
            let mut model = reader(&[ANNA, BOB]);
            model.lang = lang;
            model.names.remember(ANNA, "Anna");
            for verb_ in [
                Verb::New,
                Verb::Pick(ANNA),
                Verb::Next,
                Verb::Name("Familie".into()),
            ] {
                verb(&mut model, verb_);
            }
            assert_eq!(verb(&mut model, Verb::Create).len(), 1, "asked");
            let _ = model.take_writes();
            model
        };

        let mut old = asked(Lang::De);
        update(&mut old, UiEvent::GroupMade(Err(NotMade::HomeTooOld)));
        assert_eq!(
            old.current_toast().map(|t| t.text.as_str()),
            Some("Dein Postfach kann noch keine Gruppen führen. Aktualisiere Loquax dort.")
        );
        assert!(old.group_draft.is_none(), "nothing to ask again");
        assert_eq!(old.group(&GROUP), None);
        assert!(old.groups.is_empty());
        assert_eq!(old.conversations.len(), 2, "no row: {:?}", old.order());
        assert!(old
            .take_writes()
            .iter()
            .all(|w| !matches!(w, super::super::history::StoreWrite::Group(_))));
        let mut english = asked(Lang::En);
        update(&mut english, UiEvent::GroupMade(Err(NotMade::HomeTooOld)));
        assert_eq!(
            english.current_toast().map(|t| t.text.as_str()),
            Some("Your mailbox cannot hold groups yet. Update Loquax there.")
        );

        let mut new = asked(Lang::En);
        update(&mut new, UiEvent::GroupMade(Ok(GROUP)));
        let order = new.order();
        assert_eq!(new.conversations[order[0]].peer, GROUP, "on top");
        assert!(new.conversations[order[0]].messages.is_empty());
        let kept: Vec<GroupInfo> = new
            .take_writes()
            .into_iter()
            .filter_map(|w| match w {
                super::super::history::StoreWrite::Group(group) => Some(group),
                _ => None,
            })
            .collect();
        assert_eq!(kept.len(), 1, "written for a restart");
        assert_eq!((kept[0].name.as_str(), kept[0].ours), ("Familie", true));
    }

    /// The study's join (4.2): the invitation lists the group as an
    /// invitation, "Join" sends the group one empty message with bit 1 and
    /// no bubble, and the home's membership turns the reader joined; 2
    /// taps (the chat with the inviter, Join). Controls: a second Join
    /// sends nothing, and a membership from an address no invitation named
    /// makes no group.
    #[test]
    fn an_invitation_lists_the_group_and_join_is_one_empty_message() {
        let mut model = reader(&[LEW]);
        arrive(&mut model, BOB, 9, "", members(1, &[(ME, "Me", 0)]));
        assert!(model.group(&BOB).is_none(), "a stranger's list is nothing");

        arrive(
            &mut model,
            LEW,
            1,
            "Lew invited you to Familie",
            invite("Lew"),
        );
        let info = model.group(&GROUP).expect("invited");
        assert_eq!(info.standing, Standing::Invited);
        assert_eq!(info.inviter, Some(LEW));
        assert!(
            model.conversations.iter().any(|c| c.peer == GROUP),
            "its row"
        );
        let mut taps = 1; // the chat with Lew, open
        update(&mut model, UiEvent::Open { peer: LEW });
        let effects = verb(
            &mut model,
            Verb::Join {
                group: Target::Open,
            },
        );
        taps += 1;
        assert_eq!(taps, 2);
        let [UiEffect::Send { to, body, fields }] = effects.as_slice() else {
            panic!("one send: {effects:?}");
        };
        assert_eq!((*to, body.as_str()), (GROUP, ""));
        let lev = Lev::from_fields(fields).expect("lev");
        assert!(lev.supports_groups() && lev.members.is_none());
        assert!(is_state(body, fields), "no bubble");
        assert_eq!(model.sending.back(), Some(&(STATE_SEND, 0)));
        assert_eq!(model.conversations[model.selected].peer, GROUP);
        assert!(verb(
            &mut model,
            Verb::Join {
                group: Target::Peer(GROUP)
            }
        )
        .is_empty());

        let before = model.conversations[model.selected].unread;
        arrive(
            &mut model,
            GROUP,
            2,
            "",
            members(1, &[(LEW, "Lew", 2 | 4), (ME, "Me", 4), (ANNA, "Anna", 0)]),
        );
        let info = model.group(&GROUP).expect("in");
        assert_eq!(info.standing, Standing::Joined);
        assert_eq!(info.count(), 3);
        let group = model
            .conversations
            .iter()
            .find(|c| c.peer == GROUP)
            .expect("row");
        assert_eq!(group.unread, before, "a membership asks for no attention");
        assert!(group.messages.iter().all(Message::is_group_state));
    }

    /// The group chat (study 3.1, 6): a copy's bubble carries its writer's
    /// name and the text without the home's prefix, and two writers in a
    /// row are two groups of bubbles. Control: the same copy from an
    /// address that is no group keeps its words as they came.
    #[test]
    fn a_copy_names_its_writer_above_the_text() {
        let mut model = reader(&[LEW]);
        arrive(&mut model, LEW, 1, "invite", invite("Lew"));
        arrive(&mut model, GROUP, 2, "Lew: Brot ist da", copy(LEW));
        arrive(&mut model, GROUP, 3, "Anna: Danke", copy(ANNA));
        let c = model
            .conversations
            .iter()
            .position(|c| c.peer == GROUP)
            .expect("group");
        let bubbles = super::super::chat::bubbles(&model, c);
        let shown: Vec<_> = bubbles
            .iter()
            .filter(|b| b.invite.is_none())
            .map(|b| (b.writer.clone().map(|w| w.1), b.body.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (Some("Lew".to_string()), "Brot ist da"),
                (Some("Anna".to_string()), "Danke")
            ]
        );
        let places = super::super::chat::placements(&bubbles);
        assert!(!places[1].joins_before, "another writer, another group");

        arrive(&mut model, BOB, 4, "Lew: forged", copy(LEW));
        let b = model
            .conversations
            .iter()
            .position(|c| c.peer == BOB)
            .expect("bob");
        let bubbles = super::super::chat::bubbles(&model, b);
        assert_eq!(bubbles[0].body, "Lew: forged");
        assert!(bubbles[0].writer.is_none());
    }

    /// 1449 item 3 (walk 1 finding 9): two members of one name are told
    /// apart on the sheet by their hash tails, every row has its avatar's
    /// letters, and the reader is "You". Control: a name only one member
    /// has carries no tail.
    #[test]
    fn the_member_sheet_tells_two_of_one_name_apart_and_shows_avatars() {
        let mut model = reader(&[LEW]);
        arrive(&mut model, LEW, 1, "invite", invite("Lew"));
        arrive(
            &mut model,
            GROUP,
            2,
            "",
            members(
                2,
                &[
                    (LEW, "lew", 2 | 4),
                    (ME, "Me", 4),
                    (ANNA, "lew", 4),
                    (BOB, "Bob", 4),
                ],
            ),
        );
        verb(
            &mut model,
            Verb::Members {
                group: Target::Peer(GROUP),
            },
        );
        let sheet = super::super::group_scene::member_sheet(&model).expect("sheet");
        let names: Vec<String> = sheet
            .rows
            .iter()
            .map(|r| r.name.resolve(Lang::En))
            .collect();
        let lew = format!("lew {}", crate::names::hash_suffix(&LEW));
        let anna = format!("lew {}", crate::names::hash_suffix(&ANNA));
        assert_eq!(names, [lew.as_str(), "You", anna.as_str(), "Bob"]);
        assert_ne!(lew, anna);
        let letters: Vec<&str> = sheet
            .rows
            .iter()
            .map(|r| r.avatar.initials.as_str())
            .collect();
        assert_eq!(letters[3], "B", "{letters:?}");
        assert!(letters.iter().all(|l| !l.is_empty()), "{letters:?}");
    }

    /// The member sheet (study 4.4, 4.6): it opens from the header, Leave
    /// asks once and then sends the reader alone, flagged left, 3 taps;
    /// the admin's Remove sends the member alone, flagged left. A newer
    /// membership replaces the list, an older one does not. Controls: a
    /// member that is no admin removes nobody, and Close takes back a
    /// question before it closes the sheet.
    #[test]
    fn the_member_sheet_leaves_and_its_admin_removes() {
        let mut model = reader(&[LEW]);
        arrive(&mut model, LEW, 1, "invite", invite("Lew"));
        arrive(
            &mut model,
            GROUP,
            2,
            "",
            members(2, &[(LEW, "Lew", 2 | 4), (ME, "Me", 4), (ANNA, "Anna", 4)]),
        );
        arrive(&mut model, GROUP, 3, "", members(1, &[(LEW, "Lew", 2)]));
        assert_eq!(
            model.group(&GROUP).map(GroupInfo::count),
            Some(3),
            "older list"
        );
        let group = Target::Peer(GROUP);
        assert!(verb(
            &mut model,
            Verb::Remove {
                group,
                member: Some(ANNA),
                confirmed: true
            }
        )
        .is_empty());

        let mut taps = 0;
        verb(&mut model, Verb::Members { group });
        taps += 1;
        assert_eq!(model.group_sheet.map(|s| s.group), Some(GROUP));
        assert!(verb(
            &mut model,
            Verb::Leave {
                group,
                confirmed: false
            }
        )
        .is_empty());
        taps += 1;
        assert_eq!(
            model.group_sheet.and_then(|s| s.confirm),
            Some(Confirm::Leave)
        );
        verb(&mut model, Verb::Close);
        assert_eq!(
            model.group_sheet.map(|s| s.confirm),
            Some(None),
            "the question went"
        );
        verb(
            &mut model,
            Verb::Leave {
                group,
                confirmed: false,
            },
        );
        let effects = verb(
            &mut model,
            Verb::Leave {
                group,
                confirmed: true,
            },
        );
        taps += 1;
        assert_eq!(taps, 3);
        let [UiEffect::Send { to, fields, .. }] = effects.as_slice() else {
            panic!("one send: {effects:?}");
        };
        assert_eq!(*to, GROUP);
        let list = Lev::from_fields(fields)
            .and_then(|l| l.members)
            .expect("list");
        assert_eq!(list.members.len(), 1);
        assert_eq!(
            (list.members[0].address, list.members[0].flags),
            (ME, MEMBER_LEFT)
        );
        assert_eq!(
            model.group(&GROUP).map(|g| g.standing),
            Some(Standing::Left)
        );
        assert!(model.group_sheet.is_none());

        // The reader's own group: Remove.
        let mut admin = reader(&[ANNA]);
        admin.groups.insert(
            GROUP,
            GroupInfo {
                ours: true,
                standing: Standing::Joined,
                members: vec![GroupMember {
                    address: ANNA,
                    name: "Anna".into(),
                    admin: false,
                    joined: true,
                    left: false,
                }],
                ..GroupInfo::new(GROUP)
            },
        );
        let asked = verb(
            &mut admin,
            Verb::Remove {
                group,
                member: Some(ANNA),
                confirmed: false,
            },
        );
        assert!(asked.is_empty());
        assert_eq!(
            admin.group_sheet.and_then(|s| s.confirm),
            Some(Confirm::Remove(ANNA))
        );
        let effects = verb(
            &mut admin,
            Verb::Remove {
                group,
                member: Some(ANNA),
                confirmed: true,
            },
        );
        let [UiEffect::Send { fields, .. }] = effects.as_slice() else {
            panic!("one send: {effects:?}");
        };
        let list = Lev::from_fields(fields)
            .and_then(|l| l.members)
            .expect("list");
        assert_eq!(
            (list.members[0].address, list.members[0].flags),
            (ANNA, MEMBER_LEFT)
        );
        assert_eq!(
            admin.group(&GROUP).map(|g| g.standing),
            Some(Standing::Joined)
        );
    }

    /// 1307 G2, item 6 (study 4.5): the admin adds k friends in 3 + k
    /// taps: Members, Add members, a tick each, Add. The picker offers
    /// only friends not in the group; the home is asked once; its answer
    /// opens the member sheet. Controls: a member who is not admin gets no
    /// picker; Add with nobody ticked asks nothing; a second Add while the
    /// home has not answered asks nothing; a refusal keeps the ticks.
    #[test]
    fn the_admin_adds_members_in_three_taps_and_one_per_friend() {
        let mut model = reader(&[ANNA, BOB, LEW]);
        arrive(&mut model, LEW, 1, "invite", invite("Lew"));
        arrive(
            &mut model,
            GROUP,
            2,
            "",
            members(1, &[(ME, "Me", 2 | 4), (ANNA, "Anna", 4)]),
        );
        let group = Target::Peer(GROUP);
        let mut taps = 0;
        verb(&mut model, Verb::Members { group });
        taps += 1;
        assert!(verb(&mut model, Verb::Add { group }).is_empty());
        taps += 1;
        let candidates = model.group_candidates();
        assert!(!candidates.contains(&ANNA), "already in: {candidates:?}");
        assert!(candidates.contains(&BOB) && candidates.contains(&LEW));
        let node = super::super::group_scene::new_group(&model).expect("the picker");
        assert_eq!(
            node.next.label_key,
            super::super::strings::StrId::GroupAddConfirm
        );
        assert!(verb(&mut model, Verb::Next).is_empty(), "nobody ticked");
        for friend in [BOB, LEW] {
            verb(&mut model, Verb::Pick(friend));
            taps += 1;
        }
        let asked = verb(&mut model, Verb::Next);
        taps += 1;
        assert_eq!(taps, 3 + 2);
        let [UiEffect::AddMembers {
            group: to,
            members: added,
            ..
        }] = asked.as_slice()
        else {
            panic!("one ask of the home: {asked:?}");
        };
        assert_eq!(*to, GROUP);
        let addresses: Vec<[u8; 16]> = added.iter().map(|(p, _)| *p).collect();
        assert_eq!(addresses, [BOB, LEW]);
        assert!(verb(&mut model, Verb::Next).is_empty(), "asked once");
        update(&mut model, UiEvent::MembersAdded(Err("offline".into())));
        let draft = model.group_draft.clone().expect("kept");
        assert_eq!((draft.picked.len(), draft.asked), (2, false));
        assert_eq!(verb(&mut model, Verb::Next).len(), 1, "asked again");
        update(&mut model, UiEvent::MembersAdded(Ok(GROUP)));
        assert!(model.group_draft.is_none());
        assert_eq!(model.group_sheet.map(|s| s.group), Some(GROUP));

        let mut member = reader(&[LEW]);
        arrive(&mut member, LEW, 1, "invite", invite("Lew"));
        arrive(
            &mut member,
            GROUP,
            2,
            "",
            members(1, &[(LEW, "Lew", 2 | 4), (ME, "Me", 4)]),
        );
        assert!(verb(&mut member, Verb::Add { group }).is_empty());
        assert!(
            member.group_draft.is_none(),
            "control: no picker for a member"
        );
    }

    /// 1348: one invitation, its text English as every stock client shows
    /// it, is a card in the reader's language, said from `lev 0x04`'s
    /// inviter and group name: German on a German reader, English on an
    /// English one, and the English text stays the message's body for
    /// search and every other client.
    #[test]
    fn the_invitation_card_speaks_the_readers_language() {
        let wire = "Lew invited you to Familie: lxma://code Reply /leave to leave.";
        let card = |lang: Lang| {
            let mut model = reader(&[LEW]);
            model.lang = lang;
            arrive(&mut model, LEW, 1, wire, invite("Lew"));
            let card = super::super::group_scene::invite(&model, &GROUP, false).expect("card");
            let lew = model
                .conversations
                .iter()
                .find(|c| c.peer == LEW)
                .expect("Lew's chat");
            assert_eq!(lew.messages[0].body, wire, "the body stays the wire's");
            // 1377: the list's row (and the phone's notification) says the
            // card's title, not the wire's English with the code in it.
            let row = super::super::chat::list_row(&model, &LEW);
            assert_eq!(row.last, card.title.resolve(lang), "the row is the card");
            // 1381, finding 21: the terminal's list draws the same line,
            // no code and no wire English.
            let list: alloc::string::String = super::super::layout::list(&model, 60, 12)
                .iter()
                .map(|line| {
                    line.cells
                        .iter()
                        .map(|c| c.ch)
                        .collect::<alloc::string::String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(list.contains(&card.title.resolve(lang)), "{list}");
            assert!(!list.contains("lxma://"), "{list}");
            (card.title.resolve(lang), card.trust.resolve(lang))
        };
        let (title, trust) = card(Lang::De);
        assert_eq!(title, "Lew lädt dich zu Familie ein");
        assert_eq!(
            trust,
            "Familie liegt im Postfach von Lew. Lew könnte die Nachrichten der Gruppe lesen."
        );
        let (title, _) = card(Lang::En);
        assert_eq!(title, "Lew invited you to Familie");
    }

    /// 1307 G2, item 6: the hidden membership becomes a visible line when
    /// it changes somebody's standing: "Anna joined.", then "Bob was
    /// added. Anna left."; the window and the phone get it as a note
    /// bubble with that body, the terminal as a centred note line.
    /// Controls: the first list loaded says nothing (nothing to compare),
    /// a list at the same epoch says nothing, a list that changes nobody
    /// says nothing.
    #[test]
    fn a_membership_change_is_a_line_in_the_chat() {
        let mut model = reader(&[LEW]);
        arrive(&mut model, LEW, 1, "invite", invite("Lew"));
        arrive(
            &mut model,
            GROUP,
            2,
            "",
            members(1, &[(LEW, "Lew", 2), (ME, "Me", 4), (ANNA, "Anna", 0)]),
        );
        arrive(
            &mut model,
            GROUP,
            3,
            "",
            members(2, &[(LEW, "Lew", 2), (ME, "Me", 4), (ANNA, "Anna", 4)]),
        );
        arrive(
            &mut model,
            GROUP,
            4,
            "",
            members(2, &[(LEW, "Lew", 2), (ME, "Me", 4), (ANNA, "Anna", 4)]),
        );
        arrive(
            &mut model,
            GROUP,
            5,
            "",
            members(
                3,
                &[
                    (LEW, "Lew", 2),
                    (ME, "Me", 4),
                    (ANNA, "Anna", 1),
                    (BOB, "Bob", 0),
                ],
            ),
        );
        arrive(
            &mut model,
            GROUP,
            6,
            "",
            members(
                4,
                &[
                    (LEW, "Lew", 2),
                    (ME, "Me", 4),
                    (ANNA, "Anna", 1),
                    (BOB, "Bob", 0),
                ],
            ),
        );
        let c = model
            .conversations
            .iter()
            .position(|c| c.peer == GROUP)
            .expect("the group's chat");
        let notes: Vec<String> = super::super::chat::bubbles(&model, c)
            .into_iter()
            .filter(|b| b.note)
            .map(|b| b.body)
            .collect();
        assert_eq!(notes, ["Anna joined.", "Anna left. Bob was added."]);
        // 1348: the same memberships on a German reader say it in German,
        // from the data; the wire never carried a word of it.
        model.lang = Lang::De;
        let notes: Vec<String> = super::super::chat::bubbles(&model, c)
            .into_iter()
            .filter(|b| b.note)
            .map(|b| b.body)
            .collect();
        assert_eq!(
            notes,
            [
                "Anna ist beigetreten.",
                "Anna ist ausgetreten. Bob wurde hinzugefügt."
            ]
        );
        assert_eq!(
            membership_note(&model.conversations[c].messages, 1, Some(ANNA), Lang::De).as_deref(),
            Some("Du bist beigetreten."),
            "the reader's own line has its own words"
        );
        model.lang = Lang::En;
        let messages = &model.conversations[c].messages;
        assert_eq!(
            membership_note(messages, 0, Some(ME), Lang::En),
            None,
            "the first list"
        );
        assert_eq!(
            membership_note(messages, 2, Some(ME), Lang::En),
            None,
            "the same epoch"
        );
        assert_eq!(
            membership_note(messages, 4, Some(ME), Lang::En),
            None,
            "nobody changed"
        );
        model.selected = c;
        let rows: Vec<String> = model
            .laid()
            .rows
            .iter()
            .map(|line| line.cells.iter().map(|cell| cell.ch).collect())
            .collect();
        assert!(
            rows.iter().any(|r| r.contains("── Anna joined. ──")),
            "{rows:#?}"
        );
        assert!(rows
            .iter()
            .any(|r| r.contains("── Anna left. Bob was added. ──")));
    }

    /// 1307 G2, item 2: what the messages teach of a group is written to
    /// the store as the group's own row, and a restart lists the group
    /// from that row though none of its messages is loaded (they sit
    /// beyond the load window). Control: the same restart without the row
    /// knows no group, so the row is what carries it.
    #[test]
    fn a_restart_lists_every_group_from_the_store_without_its_messages() {
        let mut first = reader(&[LEW]);
        arrive(&mut first, LEW, 1, "invite", invite("Lew"));
        arrive(
            &mut first,
            GROUP,
            2,
            "",
            members(3, &[(LEW, "Lew", 2), (ME, "Me", 4), (ANNA, "Anna", 4)]),
        );
        let kept: Vec<GroupInfo> = first
            .take_writes()
            .into_iter()
            .filter_map(|w| match w {
                super::super::history::StoreWrite::Group(group) => Some(group),
                _ => None,
            })
            .collect();
        assert_eq!(kept.len(), 1, "one row, the newest state: {kept:?}");
        assert!(
            first
                .take_writes()
                .iter()
                .all(|w| !matches!(w, super::super::history::StoreWrite::Group(_))),
            "written once"
        );
        let row = |group: Option<GroupInfo>| super::super::history::Restored {
            peer: GROUP,
            pinned: false,
            flags: Default::default(),
            unread: 0,
            messages: Vec::new(),
            earlier: 40,
            draft: Default::default(),
            group,
            listing: None,
        };
        let mut again = reader(&[]);
        again.restore(vec![row(Some(kept[0].clone()))]);
        let group = again.group(&GROUP).expect("listed from the store");
        assert_eq!(group.standing, Standing::Joined);
        assert_eq!((group.epoch, group.count()), (3, 3));
        assert_eq!(group.name, "Familie");
        assert!(again.conversations.iter().any(|c| c.peer == GROUP));
        assert_eq!(
            again.names.label(&GROUP, again.lang).split(' ').next(),
            Some("Familie"),
            "listed under its name, not its address"
        );
        let mut without = reader(&[]);
        without.restore(vec![row(None)]);
        assert_eq!(without.group(&GROUP), None, "control: no row, no group");
    }

    /// A restart learns the groups again from the stored messages: the
    /// state messages are kept, not drawn, and no list line.
    #[test]
    fn a_restart_learns_the_groups_from_the_messages() {
        let mut first = reader(&[LEW]);
        arrive(&mut first, LEW, 1, "invite", invite("Lew"));
        arrive(&mut first, GROUP, 2, "", members(1, &[(ME, "Me", 4)]));
        let restored: Vec<_> = first
            .conversations
            .iter()
            .map(|c| super::super::history::Restored {
                peer: c.peer,
                pinned: false,
                flags: Default::default(),
                unread: 0,
                messages: c.messages.to_vec(),
                earlier: 0,
                draft: Default::default(),
                group: None,
                listing: None,
            })
            .collect();
        let mut again = reader(&[]);
        again.restore(restored);
        assert_eq!(
            again.group(&GROUP).map(|g| g.standing),
            Some(Standing::Joined)
        );
        let row = super::super::chat::list_row(&again, &GROUP);
        assert!(
            row.last.is_empty(),
            "a membership is no line: {:?}",
            row.last
        );
        assert_ne!(again.selected, NONE_OPEN);
    }

    /// The terminal's flow through the palette: `new group`, a friend by
    /// name, a pasted address, an empty line, the name: one ask with both.
    /// Controls: a word that names nobody ticks nobody, and the reader's
    /// own address is no member.
    #[test]
    fn the_palette_makes_a_group_from_names_and_pasted_addresses() {
        use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
        let mut model = reader(&[ANNA]);
        model.names.remember(ANNA, "Anna");
        let key = |model: &mut UiModel<Line>, code| {
            update(model, UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        };
        let typed = |model: &mut UiModel<Line>, text: &str| {
            for c in text.chars() {
                update(
                    model,
                    UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                );
            }
        };
        update(
            &mut model,
            UiEvent::Run(leviculum_ui::commands::Command::NewGroup),
        );
        for word in [
            "nobody",
            "anna",
            &crate::address::to_hex(&BOB),
            &crate::address::to_hex(&ME),
        ] {
            typed(&mut model, word);
            key(&mut model, KeyCode::Enter);
        }
        assert_eq!(
            model.group_draft.as_ref().map(|d| d.picked.clone()),
            Some(vec![ANNA, BOB])
        );
        key(&mut model, KeyCode::Enter);
        typed(&mut model, "Familie");
        let effects = key(&mut model, KeyCode::Enter);
        let [UiEffect::CreateGroup { name, members, .. }] = effects.as_slice() else {
            panic!("one ask: {effects:?}");
        };
        assert_eq!(name, "Familie");
        assert_eq!(members.iter().map(|m| m.0).collect::<Vec<_>>(), [ANNA, BOB]);
    }

    /// Every client says bit 1 and bit 2 (1395, not switchable); bit 0
    /// rides with the switch.
    #[test]
    fn every_client_says_it_understands_groups() {
        assert_eq!(
            client_caps(true),
            CAP_GROUPS | CAP_PROPAGATION_RECEIPTS | CAP_READ_RECEIPTS
        );
        assert_eq!(client_caps(false), CAP_GROUPS | CAP_PROPAGATION_RECEIPTS);
    }

    /// 1307 pass six, Lew's finding: opening a fresh group asked whether
    /// to save "this person" in the phone's contacts. A group is not a
    /// person, nor is an invitation card its sender's word. Control: a
    /// plain peer who wrote is offered, and so is the inviter once it
    /// writes a line of its own.
    #[test]
    fn a_group_and_an_invitation_are_no_person_for_the_address_book() {
        let mut model = reader(&[LEW, ANNA]);
        arrive(
            &mut model,
            LEW,
            1,
            "Lew invited you to Familie",
            invite("Lew"),
        );
        assert!(!model.heard_from_a_person(&LEW), "an invitation alone");
        arrive(
            &mut model,
            GROUP,
            2,
            "",
            members(1, &[(LEW, "Lew", 2 | 4), (ME, "Me", 4)]),
        );
        arrive(&mut model, GROUP, 3, "Lew: Brot ist da", copy(LEW));
        assert!(model.group(&GROUP).is_some());
        assert!(!model.heard_from_a_person(&GROUP), "a group");

        // The reader's own group, fresh: the home's membership alone.
        let mut admin = reader(&[ANNA]);
        update(&mut admin, UiEvent::GroupMade(Ok(BOB)));
        arrive(
            &mut admin,
            BOB,
            4,
            "",
            members(1, &[(ME, "Me", 2 | 4), (ANNA, "Anna", 0)]),
        );
        assert!(!admin.heard_from_a_person(&BOB), "the admin's own group");

        // Control: a person who wrote.
        arrive(&mut model, ANNA, 5, "Hallo", Lev::default());
        assert!(model.heard_from_a_person(&ANNA));
        arrive(&mut model, LEW, 6, "Kommst du?", Lev::default());
        assert!(model.heard_from_a_person(&LEW), "the inviter, writing");
        assert!(!model.heard_from_a_person(&BOB), "nobody wrote");
    }

    /// 1448 (walk finding 17): a new conversation lists the people by
    /// name, ignoring case, the archived ones too; a group and the reader
    /// are no one to start a conversation with there.
    #[test]
    fn the_people_are_every_person_by_name() {
        let mut model = reader(&[LEW, ANNA, BOB]);
        arrive(
            &mut model,
            LEW,
            1,
            "Lew invited you to Familie",
            invite("Lew"),
        );
        arrive(&mut model, ANNA, 2, "Hallo", Lev::default());
        arrive(
            &mut model,
            GROUP,
            3,
            "",
            members(1, &[(LEW, "Lew", 2 | 4), (ME, "Me", 4)]),
        );
        arrive(&mut model, GROUP, 4, "Lew: Brot ist da", copy(LEW));
        model.names.remember(LEW, "lew");
        model.names.remember(ANNA, "Anna");
        model.names.remember(BOB, "bob");
        let names = |model: &UiModel<Line>| -> Vec<String> {
            model
                .people()
                .into_iter()
                .map(|c| model.names.label(&model.conversations[c].peer, model.lang))
                .collect()
        };
        assert!(model.group(&GROUP).is_some());
        assert_eq!(names(&model), ["Anna", "bob", "lew"]);
        let bob = model
            .conversations
            .iter()
            .position(|c| c.peer == BOB)
            .expect("bob's conversation");
        model.conversations[bob].flags.archived = true;
        assert_eq!(
            names(&model),
            ["Anna", "bob", "lew"],
            "the archived one too"
        );
    }

    /// 1448 (walk finding 12): the person's first word carries its
    /// writer's time, the earliest of what it wrote, so the phone can tell
    /// a history it pulled from a contact met after it began. An
    /// invitation card and a group are no word of a person's; a model
    /// without a clock knows no time. Control: the later line does not
    /// move the first.
    #[test]
    fn a_persons_first_word_is_the_earliest_it_wrote() {
        const T: i64 = 1_790_000_000;
        let mut model = reader(&[LEW, ANNA]);
        arrive(&mut model, ANNA, 1, "ohne Uhr", Lev::default());
        assert_eq!(model.first_word_from_a_person(&ANNA), None, "no clock yet");
        update(
            &mut model,
            UiEvent::Clock(crate::when::WallTime {
                unix_secs: T,
                offset_secs: 0,
            }),
        );
        let at = |model: &mut UiModel<Line>, from, id, sent_at, lev: Lev| {
            update(
                model,
                UiEvent::Engine(OutboxEvent::Received {
                    sent_at,
                    message_id: [id; 32],
                    source: from,
                    title: Vec::new(),
                    body: b"Hallo".to_vec(),
                    fields: lev_fields(lev),
                    verified: true,
                }),
            );
        };
        at(&mut model, LEW, 2, T - 500, invite("Lew"));
        assert_eq!(model.first_word_from_a_person(&LEW), None, "an invitation");
        at(&mut model, LEW, 3, T - 300, Lev::default());
        at(&mut model, LEW, 4, T - 100, Lev::default());
        assert_eq!(
            model.first_word_from_a_person(&LEW).map(|w| w.unix_secs),
            Some(T - 300)
        );
        at(&mut model, ANNA, 5, T - 50, Lev::default());
        assert_eq!(
            model.first_word_from_a_person(&ANNA).map(|w| w.unix_secs),
            Some(T - 50),
            "the line without a clock carries no time"
        );
        assert_eq!(model.first_word_from_a_person(&BOB), None, "nobody wrote");
    }

    /// 1307 pass six: the admin's member sheet says where each invitation
    /// stands, from its delivery in the member's chat: being delivered,
    /// waiting at the propagation node for the member to come online,
    /// delivered (invited), not delivered, joined, left. Control: in a
    /// group of another's, an invited member is plainly invited.
    #[test]
    fn the_member_sheet_says_where_each_invitation_stands() {
        use leviculum_ui::delivery::DeliveryState as D;
        const KIM: [u8; 16] = [0x05; 16];
        const UWE: [u8; 16] = [0x06; 16];
        const EVA: [u8; 16] = [0x07; 16];
        const OLA: [u8; 16] = [0x08; 16];
        let mut model = reader(&[ANNA, BOB, KIM, UWE, EVA, OLA]);
        let member = |address, name: &str, joined, left| GroupMember {
            address,
            name: name.into(),
            admin: false,
            joined,
            left,
        };
        model.groups.insert(
            GROUP,
            GroupInfo {
                ours: true,
                standing: Standing::Joined,
                members: vec![
                    member(ANNA, "Anna", false, false),
                    member(BOB, "Bob", false, false),
                    member(KIM, "Kim", false, false),
                    member(UWE, "Uwe", false, false),
                    member(EVA, "Eva", true, false),
                    member(OLA, "Ola", false, true),
                ],
                ..GroupInfo::new(GROUP)
            },
        );
        let invitation = invite("Me");
        for (key, (peer, delivery)) in [
            (ANNA, Some(D::InFlight)),
            (BOB, Some(D::InMailbox)),
            (KIM, Some(D::Delivered)),
            (UWE, Some(D::Failed)),
        ]
        .into_iter()
        .enumerate()
        {
            let c = model.conversation(peer);
            model.conversations[c].messages.push(Message {
                outbound: true,
                body: "Me invited you to Familie".into(),
                id: Some([key as u8; 32]),
                delivery,
                signature: super::super::Signature::Verified,
                leg: None,
                unproven: false,
                at: None,
                size: 0,
                ledger: Vec::new(),
                last_leg: None,
                activity: key as u64,
                fields: lev_fields(invitation.clone()),
            });
        }
        let words = |model: &mut UiModel<Line>| -> Vec<(String, String)> {
            verb(
                model,
                Verb::Members {
                    group: Target::Peer(GROUP),
                },
            );
            super::super::group_scene::member_sheet(model)
                .expect("the sheet")
                .rows
                .iter()
                .map(|row| (row.name.resolve(Lang::En), row.state.resolve(Lang::En)))
                .collect()
        };
        let said = words(&mut model);
        let of = |name: &str| {
            said.iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| s.clone())
                .unwrap_or_default()
        };
        assert_eq!(of("Anna"), "invited, delivering");
        assert_eq!(of("Bob"), "invited, waiting for them to come online");
        assert_eq!(of("Kim"), "invited");
        assert_eq!(of("Uwe"), "invited, not delivered");
        assert_eq!(of("Eva"), "joined");
        assert_eq!(of("Ola"), "left");
        assert_eq!(
            super::super::group_scene::member_sheet(&model)
                .expect("the sheet")
                .rows[1]
                .state
                .resolve(Lang::De),
            "eingeladen, wartet, bis das Mitglied online ist"
        );

        // A group the reader did not make: no invitation of its own.
        model.groups.get_mut(&GROUP).expect("group").ours = false;
        for c in &mut model.conversations {
            c.messages.clear();
        }
        let said = words(&mut model);
        assert_eq!(said[0].1, "invited", "{said:?}");
    }

    /// A group the reader is in (1307 pass four): invited by Lew and
    /// joined, with Anna and Bob in it, its chat open.
    fn in_familie(admin: bool) -> UiModel<Line> {
        let mut model = reader(&[LEW]);
        arrive(&mut model, LEW, 1, "invite", invite("Lew"));
        let me = if admin { 2 | 4 } else { 4 };
        let lew = if admin { 4 } else { 2 | 4 };
        arrive(
            &mut model,
            GROUP,
            2,
            "",
            members(
                1,
                &[
                    (LEW, "Lew", lew),
                    (ME, "Me", me),
                    (ANNA, "Anna", 4),
                    (BOB, "Bob", 4),
                ],
            ),
        );
        update(&mut model, UiEvent::Open { peer: GROUP });
        model
    }

    fn group_chat(model: &UiModel<Line>) -> usize {
        model
            .conversations
            .iter()
            .position(|c| c.peer == GROUP)
            .expect("the group's chat")
    }

    /// A status of a home before 1466: five counts, no device's word.
    fn status(original: [u8; 32], delivered: u64, read: u64, readers: u64) -> Lev {
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            status: Some(GroupStatus {
                original,
                delivered,
                read,
                members: 3,
                readers,
                on_device: None,
                rungs: Vec::new(),
            }),
            ..Lev::default()
        }
    }

    /// A status since 1466: `on_device` copies on a device.
    fn status_on(
        original: [u8; 32],
        delivered: u64,
        read: u64,
        readers: u64,
        on_device: u64,
    ) -> Lev {
        let mut lev = status(original, delivered, read, readers);
        if let Some(status) = lev.status.as_mut() {
            status.on_device = Some(on_device);
        }
        lev
    }

    /// 1307 pass four, the reviewer's ticks, and 1466's truth: the group's
    /// proof of our line is one tick (the group has it), the home's status
    /// with every copy proven is one tick, arrived (a member's mailbox
    /// proves with the member's identity), every copy on a device is two,
    /// and with every member that sends receipts read the read mark; the
    /// detail view says "read by N of M" of those members alone. Controls:
    /// a status with one copy unproven moves nothing, the same status from
    /// an address that is no group moves nothing, a home before 1466 (no
    /// device count) never gives two, two of three on a device stay at one,
    /// and a late group proof takes nothing back.
    #[test]
    fn our_line_has_one_tick_from_the_group_and_two_from_its_status() {
        const OURS: [u8; 32] = [0x44; 32];
        let mut model = in_familie(false);
        update(
            &mut model,
            UiEvent::Write {
                peer: GROUP,
                body: "Brot ist da".into(),
            },
        );
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Queued { message_id: OURS }),
        );
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::State {
                message_id: OURS,
                state: leviculum_lxmf::router::MessageState::Delivered,
            }),
        );
        update(&mut model, UiEvent::SendDone(Ok(())));
        let c = group_chat(&model);
        let ours = |model: &UiModel<Line>| {
            model.conversations[c]
                .messages
                .iter()
                .find(|m| m.id == Some(OURS))
                .and_then(|m| m.delivery)
        };
        assert_eq!(ours(&model), Some(DeliveryState::InMailbox), "one tick");
        let detail = super::super::message_detail(
            &model,
            GROUP,
            model.conversations[c]
                .messages
                .iter()
                .find(|m| m.id == Some(OURS))
                .map(|m| m.activity)
                .unwrap_or_default(),
        )
        .expect("detail");
        assert!(
            detail
                .rows
                .iter()
                .any(|r| r.contains("no member's copy is confirmed yet")),
            "{:?}",
            detail.rows
        );

        arrive(&mut model, GROUP, 10, "", status(OURS, 2, 0, 2));
        assert_eq!(ours(&model), Some(DeliveryState::InMailbox), "one unproven");
        arrive(&mut model, BOB, 11, "", status(OURS, 3, 0, 2));
        assert_eq!(
            ours(&model),
            Some(DeliveryState::InMailbox),
            "not the group's word"
        );

        arrive(&mut model, GROUP, 12, "", status(OURS, 3, 1, 2));
        assert_eq!(
            ours(&model),
            Some(DeliveryState::Arrived),
            "every copy proven is one tick, arrived"
        );
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::State {
                message_id: OURS,
                state: leviculum_lxmf::router::MessageState::Delivered,
            }),
        );
        assert_eq!(ours(&model), Some(DeliveryState::Arrived), "a late proof");
        arrive(&mut model, GROUP, 14, "", status_on(OURS, 3, 1, 2, 2));
        assert_eq!(
            ours(&model),
            Some(DeliveryState::Arrived),
            "two of three devices"
        );
        arrive(&mut model, GROUP, 15, "", status_on(OURS, 3, 1, 2, 3));
        assert_eq!(ours(&model), Some(DeliveryState::Delivered), "two ticks");
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::State {
                message_id: OURS,
                state: leviculum_lxmf::router::MessageState::Delivered,
            }),
        );
        assert_eq!(ours(&model), Some(DeliveryState::Delivered), "kept");
        arrive(&mut model, GROUP, 16, "", status(OURS, 3, 1, 2));
        assert_eq!(ours(&model), Some(DeliveryState::Delivered), "never down");
        let key = model.conversations[c]
            .messages
            .iter()
            .find(|m| m.id == Some(OURS))
            .map(|m| m.activity)
            .unwrap_or_default();
        let rows = super::super::message_detail(&model, GROUP, key)
            .expect("detail")
            .rows;
        assert!(
            rows.iter().any(|r| r == "group  delivered to 3 of 3"),
            "{rows:?}"
        );
        assert!(rows.iter().any(|r| r == "read   by 1 of 2"), "{rows:?}");
        assert!(
            rows.iter().any(|r| r.contains("1 send no read receipts")),
            "{rows:?}"
        );
        // The ledger keeps the status as numbers and says them in the
        // reader's language (1381).
        assert!(
            rows.iter()
                .any(|r| r.ends_with("  Group: delivered to 3 of 3, read by 1 of 2")),
            "{rows:?}"
        );
        model.lang = Lang::De;
        let rows = super::super::message_detail(&model, GROUP, key)
            .expect("detail")
            .rows;
        assert!(
            rows.iter()
                .any(|r| r.ends_with("  Gruppe: bei 3 von 3 angekommen, von 1 von 2 gelesen")),
            "{rows:?}"
        );
        assert!(!rows.iter().any(|r| r.contains("group-status")), "{rows:?}");
        model.lang = Lang::En;
        // The phone's Info says the counts as plain rows (1381).
        let info: Vec<(String, String)> = super::super::info::message_info(&model, GROUP, key)
            .expect("info")
            .rows
            .iter()
            .map(|r| r.resolve(Lang::De))
            .collect();
        assert!(
            info.contains(&("Gruppe".into(), "Bei 3 von 3 angekommen".into())),
            "{info:?}"
        );
        assert!(
            info.contains(&(
                "Gelesen".into(),
                "Von 1 von 2 gelesen · 1 senden keine Lesebestätigungen und zählen nicht mit"
                    .into()
            )),
            "{info:?}"
        );

        arrive(&mut model, GROUP, 13, "", status(OURS, 3, 2, 2));
        assert!(
            matches!(ours(&model), Some(DeliveryState::Read { .. })),
            "read by every reader"
        );
        // The statuses are no bubbles.
        let bodies: Vec<String> = super::super::chat::bubbles(&model, c)
            .into_iter()
            .map(|b| b.body)
            .collect();
        assert_eq!(
            bodies.iter().filter(|b| b.is_empty()).count(),
            0,
            "{bodies:?}"
        );
    }

    /// 1466: a group line kept at two ticks from a status that counted
    /// proofs loads at one; one whose kept status said every copy is on a
    /// device keeps two. Controls: a line with no status (a direct
    /// message's), an incoming one, and one at read keep what they had.
    #[test]
    fn a_group_line_kept_from_before_1466_is_one_tick_unless_every_device_said_it() {
        use crate::ui::{LedgerEntry, Signature};
        let message = |outbound: bool, delivery: DeliveryState, lines: &[&str]| Message {
            outbound,
            size: 2,
            body: "hi".into(),
            id: Some([1; 32]),
            delivery: Some(delivery),
            signature: Signature::Verified,
            leg: None,
            unproven: false,
            at: None,
            ledger: lines
                .iter()
                .map(|line| LedgerEntry::new(None, line))
                .collect(),
            last_leg: None,
            activity: 1,
            fields: Vec::new(),
        };
        let after = |mut m: Message| {
            before_1466(&mut m);
            m.delivery
        };
        let two = DeliveryState::Delivered;
        let proofs = "group-status 3 3 0 2";
        let old = "the group: delivered to 3 of 3, read by 0 of 2";
        for lines in [&[proofs][..], &[old], &["group-status 3 3 0 2 2"]] {
            assert_eq!(
                after(message(true, two, lines)),
                Some(DeliveryState::Arrived),
                "{lines:?}"
            );
        }
        assert_eq!(
            after(message(true, two, &[proofs, "group-status 3 3 0 2 3"])),
            Some(two)
        );
        assert_eq!(after(message(true, two, &["queued"])), Some(two));
        assert_eq!(after(message(false, two, &[proofs])), Some(two));
        let read = DeliveryState::Read { at: 5 };
        assert_eq!(after(message(true, read, &[proofs])), Some(read));
    }

    /// 1381: a status's ledger line is data, said in the reader's
    /// language; the English sentence a store kept before is said so too.
    /// Controls: a line that only starts like one, and any other line,
    /// stay as written.
    #[test]
    fn a_status_ledger_line_is_said_in_the_readers_language() {
        let status = GroupStatus {
            original: [0; 32],
            delivered: 2,
            members: 3,
            read: 1,
            readers: 2,
            on_device: None,
            rungs: Vec::new(),
        };
        let line = status_line(&status);
        assert_eq!(line, "group-status 2 3 1 2");
        // 1466: the copies on a device follow the four.
        let devices = status_line(&GroupStatus {
            on_device: Some(1),
            ..status
        });
        assert_eq!(devices, "group-status 2 3 1 2 1");
        assert_eq!(
            ledger_line(&devices, Lang::En),
            "Group: arrived at 2 of 3, on the devices of 1, read by 1 of 2"
        );
        assert_eq!(
            ledger_line(&devices, Lang::De),
            "Gruppe: bei 2 von 3 angekommen, auf den Geräten von 1, von 1 von 2 gelesen"
        );
        assert_eq!(
            ledger_line(&line, Lang::En),
            "Group: delivered to 2 of 3, read by 1 of 2"
        );
        assert_eq!(
            ledger_line(&line, Lang::De),
            "Gruppe: bei 2 von 3 angekommen, von 1 von 2 gelesen"
        );
        assert_eq!(
            ledger_line("the group: delivered to 2 of 3, read by 1 of 2", Lang::De),
            "Gruppe: bei 2 von 3 angekommen, von 1 von 2 gelesen"
        );
        for kept in [
            "the group: delivered to 2 of 3",
            "group-status 2 3",
            "queued",
            "received, signature verified",
        ] {
            assert_eq!(ledger_line(kept, Lang::De), kept);
        }
    }

    /// 1348: a digest line the home left out carries its English sentence
    /// for a stock client and the `lev` value; the reader says it in its
    /// own language, the caption after it. Controls: an English reader
    /// shows the wire's words, a value this build does not know shows the
    /// text as it came.
    #[test]
    fn a_left_out_digest_line_is_said_in_the_readers_language() {
        const T: u64 = 1_800_000_000_000;
        let wire = left_out_wire(LEFT_OUT_PHOTO).expect("known");
        assert_eq!(wire, "sent a photo (too large for the radio)");
        let line = |sent_ms, text: &str, left_out| DigestLine {
            sender: ANNA,
            sent_ms,
            text: text.into(),
            left_out,
        };
        let lines = [
            line(T, &alloc::format!("{wire}: Seht mal"), Some(LEFT_OUT_PHOTO)),
            line(T + 1, "a message (no longer here)", Some(LEFT_OUT_GONE)),
            line(T + 2, "something new", Some(99)),
        ];
        let lev = Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            digest: Some(lines.to_vec()),
            ..Lev::default()
        };
        let body = alloc::format!("Anna {wire}: Seht mal\n: a message (no longer here)");
        let drawn = |lang: Lang| {
            let mut model = in_familie(false);
            model.lang = lang;
            arrive(&mut model, GROUP, 20, &body, lev.clone());
            let c = group_chat(&model);
            super::super::chat::bubbles(&model, c)
                .into_iter()
                .filter(|b| !b.note && !b.ours)
                .map(|b| b.body)
                .collect::<Vec<String>>()
        };
        assert_eq!(
            drawn(Lang::De),
            [
                "hat ein Foto geschickt (zu groß für die Funkstrecke): Seht mal",
                "eine Nachricht (nicht mehr da)",
                "something new",
            ]
        );
        assert_eq!(
            drawn(Lang::En),
            [
                "sent a photo (too large for the radio): Seht mal",
                "a message (no longer here)",
                "something new",
            ]
        );
    }

    fn digest(lines: &[([u8; 16], u64, &str)]) -> Lev {
        Lev {
            capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
            digest: Some(
                lines
                    .iter()
                    .map(|(sender, sent_ms, text)| leviculum_lxmf::lev::DigestLine {
                        sender: *sender,
                        sent_ms: *sent_ms,
                        text: (*text).into(),
                        left_out: None,
                    })
                    .collect(),
            ),
            ..Lev::default()
        }
    }

    fn copy_at(writer: [u8; 16], sent_ms: u64) -> Lev {
        Lev {
            capabilities: Some(CAP_GROUPS),
            group_copy: Some(GroupCopy {
                sender: writer,
                sent_ms,
                epoch: 1,
            }),
            ..Lev::default()
        }
    }

    /// 1307 pass four, the digest drawn and the copies deduplicated (study
    /// 5.3, 6.1, 6.2): a digest is one bubble a line, under its writer's
    /// name and at the writer's time; a copy of a line the digest already
    /// carried is not shown again; a line older than one shown before it
    /// is marked as arrived late. Controls: the digest's text without the
    /// `0x06` (a stock member's) stays one bubble, and a copy newer than
    /// everything before it is not late.
    #[test]
    fn a_digest_is_a_bubble_a_writer_and_a_line_is_shown_once() {
        const T: u64 = 1_800_000_000_000;
        let mut model = in_familie(false);
        arrive(
            &mut model,
            GROUP,
            20,
            "Anna: Brot\nBob: Danke",
            digest(&[(ANNA, T, "Brot"), (BOB, T + 2_000, "Danke")]),
        );
        // Bob's line once more, as a copy of its own (a second acceptance).
        arrive(&mut model, GROUP, 21, "Bob: Danke", copy_at(BOB, T + 2_000));
        // Anna's newer line, alone: not late.
        arrive(
            &mut model,
            GROUP,
            22,
            "Anna: Bis gleich",
            copy_at(ANNA, T + 9_000),
        );
        // A line of Bob's from before it, retried late.
        arrive(
            &mut model,
            GROUP,
            23,
            "Bob: Wer kommt?",
            copy_at(BOB, T + 5_000),
        );
        let c = group_chat(&model);
        type Drawn = (Option<[u8; 16]>, String, bool, Option<i64>);
        let drawn: Vec<Drawn> = super::super::chat::bubbles(&model, c)
            .into_iter()
            .filter(|b| !b.note && !b.ours)
            .map(|b| {
                (
                    b.writer.map(|w| w.0),
                    b.body,
                    b.late,
                    b.at.map(|a| a.unix_secs),
                )
            })
            .collect();
        assert_eq!(
            drawn,
            [
                (Some(ANNA), "Brot".to_string(), false, Some(1_800_000_000)),
                (Some(BOB), "Danke".to_string(), false, Some(1_800_000_002)),
                (Some(ANNA), "Bis gleich".to_string(), false, None),
                (Some(BOB), "Wer kommt?".to_string(), true, None),
            ]
            .map(|(w, b, l, at)| (w, b, l, at.or(model.now.map(|n| n.unix_secs)))),
            "Bob's second copy is not drawn"
        );
        let names: Vec<String> = super::super::chat::bubbles(&model, c)
            .into_iter()
            .filter_map(|b| b.writer.map(|w| w.1))
            .take(2)
            .collect();
        assert_eq!(names, ["Anna", "Bob"], "the admin's names");

        // Control: a stock member's digest, the text alone, is one bubble.
        let mut stock = in_familie(false);
        arrive(
            &mut stock,
            GROUP,
            20,
            "Anna: Brot\nBob: Danke",
            Lev::default(),
        );
        let c = group_chat(&stock);
        let bodies: Vec<String> = super::super::chat::bubbles(&stock, c)
            .into_iter()
            .filter(|b| !b.note)
            .map(|b| b.body)
            .collect();
        assert_eq!(bodies, ["Anna: Brot\nBob: Danke"]);
    }

    /// 1388: the bubble takes the writer from `0x02` and strips exactly
    /// the name the membership gives them, so a name with a colon and a
    /// line that starts with one stay whole; a photo without words, whose
    /// copy says "Photo from a:b", is the photo alone under its writer.
    /// Controls: a writer the membership does not name falls back to the
    /// split at the first colon, and a stock reader (no `0x02`) keeps the
    /// text as it came.
    #[test]
    fn a_copy_loses_exactly_its_writers_name_and_a_photo_its_sentence() {
        const CAROL: [u8; 16] = [0x05; 16];
        let mut model = in_familie(false);
        arrive(
            &mut model,
            GROUP,
            3,
            "",
            members(
                2,
                &[
                    (LEW, "Lew", 2 | 4),
                    (ME, "Me", 4),
                    (ANNA, "a:b", 4),
                    (BOB, "Bob", 4),
                ],
            ),
        );
        arrive(&mut model, GROUP, 20, "a:b: hello", copy_at(ANNA, 10));
        arrive(&mut model, GROUP, 21, "Bob: : smile", copy_at(BOB, 11));
        let photo = crate::ui::photos::photo_fields("webp", vec![1, 2, 3]);
        let mut fields = lev_fields(copy_at(ANNA, 12));
        fields.extend(photo.iter().cloned());
        update(
            &mut model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [22; 32],
                source: GROUP,
                title: Vec::new(),
                body: b"Photo from a:b".to_vec(),
                fields,
                verified: true,
            }),
        );
        // A writer the membership does not name: the split.
        arrive(
            &mut model,
            GROUP,
            23,
            "Carol: hi: there",
            copy_at(CAROL, 13),
        );
        let c = group_chat(&model);
        let drawn: Vec<(String, String, bool)> = super::super::chat::bubbles(&model, c)
            .into_iter()
            .filter(|b| !b.note && !b.ours)
            .map(|b| {
                (
                    b.writer.map(|w| w.1).unwrap_or_default(),
                    b.body,
                    b.photo.is_some(),
                )
            })
            .collect();
        assert_eq!(
            drawn,
            [
                ("a:b", "hello", false),
                ("Bob", ": smile", false),
                ("a:b", "", true),
                ("Carol", "hi: there", false),
            ]
            .map(|(w, b, p)| (w.to_string(), b.to_string(), p))
        );

        // The terminal names the writer over the photo as every copy does
        // there, `"a:b:"`, and never says the sentence.
        let rows: Vec<String> = super::super::view(&model)
            .messages
            .iter()
            .map(|row| row.cells.iter().map(|c| c.ch).collect::<String>())
            .collect();
        assert!(
            rows.iter()
                .any(|row| row.trim_start().starts_with("a:b:") && !row.contains("hello")),
            "{rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("Photo from")),
            "{rows:#?}"
        );

        // Control: a stock reader's copy has no 0x02, its text stays.
        let mut stock = in_familie(false);
        arrive(&mut stock, GROUP, 20, "Photo from a:b", Lev::default());
        let c = group_chat(&stock);
        let bodies: Vec<String> = super::super::chat::bubbles(&stock, c)
            .into_iter()
            .filter(|b| !b.note)
            .map(|b| b.body)
            .collect();
        assert_eq!(bodies, ["Photo from a:b"]);
    }

    fn ended(epoch: u64, list: &[([u8; 16], &str, u64)]) -> Lev {
        let mut lev = members(epoch, list);
        if let Some(members) = lev.members.as_mut() {
            members.ended = true;
        }
        lev
    }

    /// 1307 pass four, the owner deletes the group: Members, Delete group,
    /// Yes is 3 taps and one ask of the home; its answer takes the group
    /// and every message of it off the device. On a member's device the
    /// home's last membership ends it: the history stays, the line "Lew
    /// ended the group." is drawn, compose is closed (a send is refused and
    /// the scene draws the line where the bar was), Leave is "Remove from
    /// list", and that removes it. Controls: a member's sheet has no
    /// Delete, and its Delete verb asks nothing; the owner's other device,
    /// told by the same membership, forgets the group too.
    #[test]
    fn the_owner_deletes_a_group_and_a_member_keeps_it_closed() {
        // The owner.
        let mut owner = in_familie(true);
        arrive(&mut owner, GROUP, 30, "Anna: Hallo", copy(ANNA));
        let mut taps = 0;
        verb(
            &mut owner,
            Verb::Members {
                group: Target::Open,
            },
        );
        taps += 1;
        let sheet = super::super::group_scene::member_sheet(&owner).expect("sheet");
        let delete = sheet.delete.expect("the admin's Delete");
        let Intent::Group(delete) = delete.intent else {
            panic!("a group verb");
        };
        assert!(verb(&mut owner, delete).is_empty(), "asked first");
        taps += 1;
        let ask = super::super::group_scene::member_sheet(&owner)
            .and_then(|s| s.ask)
            .expect("the question");
        let Intent::Group(yes) = ask.yes.intent else {
            panic!("a group verb");
        };
        let effects = verb(&mut owner, yes);
        taps += 1;
        assert_eq!(taps, 3);
        assert_eq!(effects, [UiEffect::DeleteGroup { group: GROUP }]);
        assert!(owner.group(&GROUP).is_some(), "until the home answers");
        let _ = owner.take_writes();
        update(&mut owner, UiEvent::GroupDeleted(Ok(GROUP)));
        assert!(owner.group(&GROUP).is_none());
        assert!(owner.conversations.iter().all(|c| c.peer != GROUP));
        assert!(owner.take_writes().iter().any(|w| matches!(
            w,
            super::super::history::StoreWrite::ForgetConversation { peer } if *peer == GROUP
        )));
        // The owner's other device learns it from the home's membership.
        let mut other = in_familie(true);
        arrive(
            &mut other,
            GROUP,
            31,
            "",
            ended(
                2,
                &[(LEW, "Lew", 1), (ME, "Me", 2 | 4 | 1), (ANNA, "Anna", 5)],
            ),
        );
        assert!(other.group(&GROUP).is_none());
        assert!(other.conversations.iter().all(|c| c.peer != GROUP));

        // A member.
        let mut member = in_familie(false);
        arrive(&mut member, GROUP, 30, "Anna: Hallo", copy(ANNA));
        verb(
            &mut member,
            Verb::Members {
                group: Target::Open,
            },
        );
        let sheet = super::super::group_scene::member_sheet(&member).expect("sheet");
        assert!(sheet.delete.is_none(), "control: no Delete for a member");
        assert!(
            verb(
                &mut member,
                Verb::Delete {
                    group: Target::Open,
                    confirmed: true
                }
            )
            .is_empty(),
            "control: a member's Delete asks nothing"
        );
        verb(&mut member, Verb::Close);
        arrive(
            &mut member,
            GROUP,
            31,
            "",
            ended(
                2,
                &[
                    (LEW, "Lew", 2 | 1),
                    (ME, "Me", 5),
                    (ANNA, "Anna", 5),
                    (BOB, "Bob", 5),
                ],
            ),
        );
        let info = member.group(&GROUP).expect("kept");
        assert_eq!(info.standing, Standing::Ended);
        let c = group_chat(&member);
        let bubbles = super::super::chat::bubbles(&member, c);
        assert!(
            bubbles.iter().any(|b| b.body == "Hallo"),
            "the history stays"
        );
        assert!(
            bubbles
                .iter()
                .any(|b| b.note && b.body == "Lew ended the group."),
            "{:?}",
            bubbles.iter().map(|b| &b.body).collect::<Vec<_>>()
        );
        // 1348: the ended line in the reader's language, from the data.
        member.lang = Lang::De;
        assert!(super::super::chat::bubbles(&member, c)
            .iter()
            .any(|b| b.note && b.body == "Lew hat die Gruppe beendet."));
        member.lang = Lang::En;
        let chat = super::super::scene::chat(&member, c);
        assert_eq!(
            chat.ended.map(|t| t.resolve(Lang::En)),
            Some("Lew ended the group".to_string())
        );
        let before = member.conversations[c].messages.len();
        assert!(
            verb_send(&mut member, "noch da?").is_empty(),
            "compose is closed"
        );
        assert_eq!(member.conversations[c].messages.len(), before);
        verb(
            &mut member,
            Verb::Members {
                group: Target::Open,
            },
        );
        let sheet = super::super::group_scene::member_sheet(&member).expect("sheet");
        let remove = sheet.leave.expect("Remove from list");
        assert_eq!(remove.label_key, StrId::GroupRemoveFromList);
        assert!(sheet.again.is_some(), "and Start again");
        let Intent::Group(remove) = remove.intent else {
            panic!("a group verb");
        };
        // Nobody to tell; what the group's messages saved here goes (1662).
        assert_eq!(
            verb(&mut member, remove),
            vec![UiEffect::ForgetSaved {
                peer: GROUP,
                key: None
            }],
            "nobody to tell"
        );
        assert!(member.group(&GROUP).is_none());
        assert!(member.conversations.iter().all(|c| c.peer != GROUP));
    }

    fn verb_send(model: &mut UiModel<Line>, body: &str) -> Vec<UiEffect> {
        update(
            model,
            UiEvent::Write {
                peer: GROUP,
                body: body.into(),
            },
        )
    }

    /// Study 4.7, "Start again with the same people": from the sheet of a
    /// group that ended, one tap opens "New group" at its name with the
    /// same people ticked and the old name, and Create asks the home: 2
    /// taps. Controls: a group the reader is still in offers no Start
    /// again, and its verb opens nothing.
    #[test]
    fn an_ended_group_starts_again_with_the_same_people_in_two_taps() {
        let mut model = in_familie(false);
        verb(
            &mut model,
            Verb::Members {
                group: Target::Open,
            },
        );
        assert!(
            super::super::group_scene::member_sheet(&model)
                .expect("sheet")
                .again
                .is_none(),
            "control: in it, nothing to start again"
        );
        verb(
            &mut model,
            Verb::Again {
                group: Target::Open,
            },
        );
        assert!(
            model.group_draft.is_none(),
            "control: the verb opens nothing"
        );
        verb(&mut model, Verb::Close);
        arrive(
            &mut model,
            GROUP,
            31,
            "",
            ended(
                2,
                &[
                    (LEW, "Lew", 2 | 1),
                    (ME, "Me", 5),
                    (ANNA, "Anna", 5),
                    (BOB, "Bob", 5),
                ],
            ),
        );
        verb(
            &mut model,
            Verb::Members {
                group: Target::Open,
            },
        );
        let again = super::super::group_scene::member_sheet(&model)
            .and_then(|s| s.again)
            .expect("Start again");
        let Intent::Group(again) = again.intent else {
            panic!("a group verb");
        };
        let mut taps = 0;
        verb(&mut model, again);
        taps += 1;
        let draft = model.group_draft.clone().expect("the draft");
        assert_eq!(draft.step, Step::Name);
        assert_eq!(draft.name, "Familie");
        assert_eq!(draft.picked, [LEW, ANNA, BOB]);
        let effects = verb(&mut model, Verb::Create);
        taps += 1;
        assert_eq!(taps, 2);
        let [UiEffect::CreateGroup { name, members, .. }] = effects.as_slice() else {
            panic!("one ask: {effects:?}");
        };
        assert_eq!(name, "Familie");
        assert_eq!(members.len(), 3);
    }

    /// 1307 pass four, the terminal's way: `delete group` asks in the
    /// palette and Enter asks the home; `start again` on an ended group
    /// opens its name, and an empty Enter keeps the old one. Controls: a
    /// member's `delete group` asks nothing and says why, and `start again`
    /// in a group the reader is in opens nothing.
    #[test]
    fn the_palette_deletes_and_starts_again() {
        use leviculum_ui::commands::Command;
        use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
        let enter = |model: &mut UiModel<Line>| {
            update(
                model,
                UiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            )
        };
        let mut owner = in_familie(true);
        update(&mut owner, UiEvent::Run(Command::DeleteGroup));
        assert_eq!(
            owner.palette.as_ref().and_then(|p| p.ask),
            Some(Ask::DeleteGroup(GROUP))
        );
        assert_eq!(enter(&mut owner), [UiEffect::DeleteGroup { group: GROUP }]);

        let mut member = in_familie(false);
        assert!(update(&mut member, UiEvent::Run(Command::DeleteGroup)).is_empty());
        assert!(member.palette.as_ref().and_then(|p| p.ask).is_none());
        assert!(member.current_toast().is_some());
        update(&mut member, UiEvent::Run(Command::StartAgain));
        assert!(member.group_draft.is_none(), "control: still in it");
        arrive(
            &mut member,
            GROUP,
            31,
            "",
            ended(2, &[(LEW, "Lew", 2 | 1), (ME, "Me", 5), (ANNA, "Anna", 5)]),
        );
        update(&mut member, UiEvent::Run(Command::StartAgain));
        let effects = enter(&mut member);
        let [UiEffect::CreateGroup { name, members, .. }] = effects.as_slice() else {
            panic!("one ask: {effects:?}");
        };
        assert_eq!(name, "Familie");
        assert_eq!(members.iter().map(|m| m.0).collect::<Vec<_>>(), [LEW, ANNA]);
    }
}
