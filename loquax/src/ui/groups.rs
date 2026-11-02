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

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_lxmf::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE};
use leviculum_lxmf::lev::{
    Lev, Member, Members, CAP_GROUPS, CAP_READ_RECEIPTS, MEMBER_ADMIN, MEMBER_JOINED, MEMBER_LEFT,
};
use leviculum_lxmf::Field;

use super::contacts::Ask;
use super::intent::Target;
use super::{Message, Palette, UiEffect, UiModel};

/// The slot of [`UiModel::sending`] a group's state takes (a Join, a
/// Leave, a Remove): it runs in the shell's queue like every send, and no
/// bubble follows it, as a receipt's own.
pub const STATE_SEND: usize = usize::MAX - 1;

/// The capabilities every Loquax client says on every message it sends
/// (1307): bit 1, it understands groups, always; bit 0, read receipts,
/// while the reader's switch is on.
pub fn client_caps(receipts: bool) -> u64 {
    CAP_GROUPS | if receipts { CAP_READ_RECEIPTS } else { 0 }
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
    fn new(address: [u8; 16]) -> Self {
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
}

/// What the member sheet asks before it acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    Leave,
    Remove([u8; 16]),
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
    /// The admin's "Remove" on a member: asked first unless `confirmed`;
    /// with no member the palette asks for its name.
    Remove {
        group: Target,
        member: Option<[u8; 16]>,
        confirmed: bool,
    },
    /// The picker, the sheet or its question closes.
    Close,
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
pub fn copy_of<'a>(body: &'a str, fields: &[Field]) -> Option<([u8; 16], &'a str, &'a str)> {
    let copy = Lev::from_fields(fields)?.group_copy?;
    let (name, text) = match body.split_once(':') {
        Some((name, rest)) => (name, rest.strip_prefix(' ').unwrap_or(rest)),
        None => ("", body),
    };
    Some((copy.sender, name, text))
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

    /// The friends the "New group" picker lists: every conversation the
    /// list shows that is no group, in the list's order.
    pub fn group_candidates(&self) -> Vec<[u8; 16]> {
        self.order()
            .into_iter()
            .map(|c| self.conversations[c].peer)
            .filter(|peer| !self.groups.contains_key(peer) && self.address != Some(*peer))
            .collect()
    }

    /// Learn what a message in `peer`'s conversation says of a group:
    /// `outbound` when the reader (or another of its devices) sent it.
    pub(super) fn learn_group(&mut self, peer: [u8; 16], outbound: bool, fields: &[Field]) {
        let Some(lev) = Lev::from_fields(fields) else {
            return;
        };
        let own = self.address;
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
                group.members = members
                    .members
                    .iter()
                    .map(|m| GroupMember {
                        address: m.address,
                        name: m.name.clone(),
                        admin: m.flags & MEMBER_ADMIN != 0,
                        joined: m.flags & (MEMBER_JOINED | MEMBER_ADMIN) != 0,
                        left: m.flags & MEMBER_LEFT != 0,
                    })
                    .collect();
                if let Some(me) = own.and_then(|own| group.member(&own)).cloned() {
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
                .map(|p| self.names.label(p))
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
                    "tick friends: type a name, Enter ticks or unticks it".to_string(),
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
                self.names.label(&group)
            ),],
            Ask::RemoveMember(group) => alloc::vec![alloc::format!(
                "remove whom from {}? type a name, Enter",
                self.names.label(&group)
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
            None => self.names.label(&member.address),
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
                        alloc::format!("no friend called {typed}"),
                    ),
                }
                self.palette = Some(Palette {
                    ask: Some(Ask::GroupPick),
                    ..Palette::default()
                });
                Vec::new()
            }
            Ask::GroupName => {
                self.group_verb(Verb::Name(typed));
                self.group_verb(Verb::Create)
            }
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
        self.group_candidates()
            .into_iter()
            .find(|peer| self.names.label(peer).to_lowercase().contains(&lower))
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

    /// The home's answer to Create: the group's conversation is listed
    /// under its name and opened, or the reader is told why not.
    pub(super) fn group_made(&mut self, made: Result<[u8; 16], String>) -> Vec<UiEffect> {
        let Some(draft) = self.group_draft.take() else {
            return Vec::new();
        };
        let address = match made {
            Ok(address) => address,
            Err(why) => {
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
                    name: self.names.label(peer),
                    admin: false,
                    joined: false,
                    left: false,
                }))
                .collect();
        }
        self.names.remember(address, &name);
        let index = self.conversation(address);
        self.select(index);
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
        let [UiEffect::CreateGroup { name, members }] = effects.as_slice() else {
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

        update(&mut model, UiEvent::GroupMade(Err("no mailbox".into())));
        assert!(model.group_draft.is_some(), "kept for another try");
        assert!(model.current_toast().is_some());
        assert_eq!(verb(&mut model, Verb::Create).len(), 1, "asked again");
        update(&mut model, UiEvent::GroupMade(Ok(GROUP)));
        assert!(model.group_draft.is_none());
        let open = &model.conversations[model.selected];
        assert_eq!(open.peer, GROUP);
        assert_eq!(model.names.label(&GROUP).split(' ').next(), Some("Familie"));
        let info = model.group(&GROUP).expect("the group");
        assert!(info.ours);
        assert_eq!(info.standing, Standing::Joined);
        assert_eq!(info.count(), 3, "the reader and two friends");
        assert!(
            !model.group_candidates().contains(&GROUP),
            "no group in a group"
        );
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
                messages: c.messages.clone(),
                earlier: 0,
                draft: Default::default(),
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
        let [UiEffect::CreateGroup { name, members }] = effects.as_slice() else {
            panic!("one ask: {effects:?}");
        };
        assert_eq!(name, "Familie");
        assert_eq!(members.iter().map(|m| m.0).collect::<Vec<_>>(), [ANNA, BOB]);
    }

    /// Every client says bit 1; bit 0 rides with the switch.
    #[test]
    fn every_client_says_it_understands_groups() {
        assert_eq!(client_caps(true), CAP_GROUPS | CAP_READ_RECEIPTS);
        assert_eq!(client_caps(false), CAP_GROUPS);
    }
}
