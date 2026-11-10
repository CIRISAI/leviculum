//! The scene's group nodes (1307 pass two, the study's sections 3.0 and
//! 4): the row's member count, the invitation card with its Join, the
//! sender above a bubble that is not the reader's, the member sheet, and
//! "New group" with its picker and its name. Every target draws them from
//! here; what a tap does is the node's [`Action`], a [`Verb`] the model
//! folds ([`super::UiEvent::Group`]).
//!
//! A scene with no group in it carries none of these nodes, and its JSON
//! carries none of their keys, so the goldens of a plain list and chat
//! stay as they were.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{json, Value};

use super::groups::{Confirm, GroupInfo, Standing, Step, Verb};
use super::intent::{Intent, Target};
use super::scene::{A11y, Action, Avatar, NodeId};
use super::strings::{Arg, StrId, Text};
use super::{chat, UiModel};
use leviculum_ui::scene::{actions_json as actions, Role, ToJson};

/// A group's row and chat bar: how many are in it, and whether the reader
/// is only invited yet.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupBadge {
    pub members: usize,
    pub standing: Standing,
    /// `3 members`, or `Invitation` before the Join.
    pub words: Text,
}

/// An invitation (`lev 0x04`) as a card: in the inviter's chat, the bubble
/// that carries it, and in the group's own chat until the Join.
#[derive(Clone, Debug, PartialEq)]
pub struct InviteNode {
    pub id: NodeId,
    pub group: [u8; 16],
    /// "Lew invited you to Familie", or ours "You invited them to …".
    pub title: Text,
    /// The trust sentence of the study's 3.0, read once before the Join.
    pub trust: Text,
    /// Join, while the reader is only invited.
    pub join: Option<Action>,
    /// "You joined Familie" once joined, "You left Familie" once left.
    pub state: Option<Text>,
    pub a11y: A11y,
}

/// One friend in the "New group" picker.
#[derive(Clone, Debug, PartialEq)]
pub struct PickRow {
    pub id: NodeId,
    pub peer: [u8; 16],
    pub name: String,
    pub avatar: Avatar,
    pub picked: bool,
    pub toggle: Action,
    pub a11y: A11y,
}

/// "New group": the picker, then the name with the trust sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct NewGroupNode {
    pub id: NodeId,
    pub step: Step,
    pub title: Text,
    /// The friends, every step: the name step shows the ticked ones.
    pub rows: Vec<PickRow>,
    /// `2 selected`.
    pub picked: Text,
    /// Next: enabled once a friend is ticked.
    pub next: Action,
    /// The name field's hint and what it holds (the platform's text).
    pub hint: Text,
    pub name: String,
    /// The trust sentence under the name field.
    pub trust: Text,
    /// Create: enabled once a name is typed, and while the home has not
    /// been asked.
    pub create: Action,
    pub close: Action,
    pub a11y: A11y,
}

/// One member in the sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberRow {
    pub id: NodeId,
    pub address: [u8; 16],
    pub name: Text,
    /// `admin`, `joined`, `invited`, `left`.
    pub state: Text,
    pub you: bool,
    /// Its picture, the list's avatar (1449 item 3, walk 1 finding 9).
    pub avatar: Avatar,
    /// The admin's Remove, on every other present member: a renderer
    /// offers it in the row's menu, never on a tap of the row, and the
    /// sheet asks before it acts.
    pub remove: Option<Action>,
    pub a11y: A11y,
}

/// What the sheet asks before it acts.
#[derive(Clone, Debug, PartialEq)]
pub struct AskNode {
    pub words: Text,
    pub yes: Action,
    pub no: Action,
}

/// A group's member sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberSheet {
    pub id: NodeId,
    pub group: [u8; 16],
    pub title: Text,
    pub count: Text,
    pub rows: Vec<MemberRow>,
    /// Leave group, while the reader is in it.
    pub leave: Option<Action>,
    /// Add members, the admin's (1307 G2, study 4.5).
    pub add: Option<Action>,
    /// Delete group, the admin's (1307 pass four): for everyone.
    pub delete: Option<Action>,
    /// Start again with the same people, in a group the reader left or
    /// that ended (study 4.7).
    pub again: Option<Action>,
    pub ask: Option<AskNode>,
    pub close: Action,
    pub a11y: A11y,
}

fn verb(verb: Verb) -> Action {
    Action::of(Intent::Group(verb))
}

fn enabled(mut action: Action, on: bool) -> Action {
    action.enabled = on;
    action
}

/// The row's and the bar's badge of the group at `peer`.
pub fn badge<B>(model: &UiModel<B>, peer: &[u8; 16]) -> Option<GroupBadge> {
    let info = model.group(peer)?;
    let members = info.count();
    let words = match info.standing {
        Standing::Invited => Text::key(StrId::GroupInvitation),
        _ => Text::Key(
            StrId::GroupMemberCount,
            Vec::from([Arg::Int(members as i64)]),
        ),
    };
    Some(GroupBadge {
        members,
        standing: info.standing,
        words,
    })
}

/// The line a group its admin deleted shows where its compose bar was
/// (1307 pass four): "Lew ended the group", the admin as the newest
/// membership names it. `None` for any other conversation.
pub fn ended<B>(model: &UiModel<B>, peer: &[u8; 16]) -> Option<Text> {
    let info = model
        .group(peer)
        .filter(|g| g.standing == Standing::Ended)?;
    let admin = info
        .members
        .iter()
        .find(|m| m.admin)
        .map(|m| model.member_name(m))
        .unwrap_or_else(|| admin_name(model, info));
    Some(Text::Key(StrId::GroupEnded, Vec::from([Arg::Str(admin)])))
}

/// The admin as the reader knows it: its name for the inviter, else the
/// invitation's word.
fn admin_name<B>(model: &UiModel<B>, info: &GroupInfo) -> String {
    match info.inviter.and_then(|inviter| model.names.shown(&inviter)) {
        Some(shown) => shown.into(),
        None if !info.admin_name.is_empty() => info.admin_name.clone(),
        None => info
            .inviter
            .map(|i| model.names.label(&i, model.lang))
            .unwrap_or_default(),
    }
}

/// The initials a group's avatar shows: how many are in it, its name's
/// before a membership came.
pub fn initials<B>(model: &UiModel<B>, peer: &[u8; 16], badge: &GroupBadge) -> String {
    match badge.members {
        0 => chat::initials(model.names.shown(peer), peer),
        n => n.to_string(),
    }
}

/// The card of the invitation to `group`; `ours` for one the reader sent.
pub fn invite<B>(model: &UiModel<B>, group: &[u8; 16], ours: bool) -> Option<InviteNode> {
    let info = model.group(group)?;
    let id = NodeId::Join(*group);
    let name = info.name.clone();
    let admin = admin_name(model, info);
    let (title, trust) = match ours || info.ours {
        true => (
            Text::Key(StrId::GroupYouInvited, Vec::from([Arg::Str(name.clone())])),
            Text::Key(StrId::GroupTrustCreate, Vec::from([Arg::Str(name.clone())])),
        ),
        false => (
            Text::Key(
                StrId::GroupInvited,
                Vec::from([Arg::Str(admin.clone()), Arg::Str(name.clone())]),
            ),
            Text::Key(
                StrId::GroupTrustJoin,
                Vec::from([Arg::Str(name.clone()), Arg::Str(admin)]),
            ),
        ),
    };
    let (join, state) = match (info.standing, ours || info.ours) {
        (Standing::Invited, false) => (
            Some(verb(Verb::Join {
                group: Target::Peer(*group),
            })),
            None,
        ),
        (Standing::Left, _) => (
            None,
            Some(Text::Key(StrId::GroupYouLeft, Vec::from([Arg::Str(name)]))),
        ),
        (_, false) => (
            None,
            Some(Text::Key(StrId::GroupJoined, Vec::from([Arg::Str(name)]))),
        ),
        (_, true) => (None, None),
    };
    Some(InviteNode {
        id,
        group: *group,
        a11y: A11y::of(id, Role::Card, title.clone()),
        title,
        trust,
        join,
        state,
    })
}

/// "New group", while it is open.
pub fn new_group<B>(model: &UiModel<B>) -> Option<NewGroupNode> {
    let draft = model.group_draft.as_ref()?;
    let id = NodeId::NewGroup;
    let peers: Vec<[u8; 16]> = match draft.step {
        Step::Pick => model.group_candidates(),
        Step::Name => draft.picked.clone(),
    };
    let rows = peers
        .into_iter()
        .map(|peer| {
            let picked = draft.picked.contains(&peer);
            let name = model.names.label(&peer, model.lang);
            let id = NodeId::PickRow(peer);
            let mut a11y = A11y::of(id, Role::Row, Text::Raw(name.clone()));
            a11y.selected = picked;
            PickRow {
                id,
                peer,
                avatar: Avatar::of(
                    chat::initials(model.names.shown(&peer), &peer),
                    &peer,
                    None,
                    None,
                ),
                name,
                picked,
                toggle: verb(Verb::Pick(peer)),
                a11y,
            }
        })
        .collect();
    let named = draft.name.trim();
    // Adding to a group (G2): "Add members", and Next is "Add".
    let title = match (draft.step, draft.to) {
        (Step::Pick, Some(_)) => Text::key(StrId::GroupAdd),
        (Step::Pick, None) => Text::key(StrId::GroupPickTitle),
        (Step::Name, _) => Text::key(StrId::NewGroup),
    };
    let mut next = enabled(verb(Verb::Next), !draft.picked.is_empty() && !draft.asked);
    if draft.to.is_some() {
        next.label_key = StrId::GroupAddConfirm;
    }
    let trust_name = match named.is_empty() {
        true => String::from("…"),
        false => String::from(named),
    };
    Some(NewGroupNode {
        id,
        step: draft.step,
        a11y: A11y::of(id, Role::Sheet, title.clone()),
        title,
        rows,
        picked: Text::Key(
            StrId::GroupPicked,
            Vec::from([Arg::Int(draft.picked.len() as i64)]),
        ),
        next,
        hint: Text::key(StrId::GroupNameHint),
        name: draft.name.clone(),
        trust: Text::Key(StrId::GroupTrustCreate, Vec::from([Arg::Str(trust_name)])),
        create: enabled(
            verb(Verb::Create),
            !named.is_empty() && !draft.asked && draft.step == Step::Name,
        ),
        close: verb(Verb::Close),
    })
}

/// The member sheet, while it is open.
pub fn member_sheet<B>(model: &UiModel<B>) -> Option<MemberSheet> {
    let sheet = model.group_sheet?;
    let info = model.group(&sheet.group)?;
    let group = sheet.group;
    let id = NodeId::Members;
    // Two members of one name are told apart by their hash tails, as the
    // list does (1452), here among the sheet's own rows (1449 item 3, walk
    // 1 finding 9: two "lew" rows).
    let shown: Vec<String> = info.members.iter().map(|m| model.member_name(m)).collect();
    let rows = info
        .members
        .iter()
        .zip(&shown)
        .map(|(m, plain)| {
            let you = model.address == Some(m.address);
            let twice = shown.iter().filter(|other| *other == plain).count() > 1;
            let name = match (you, twice) {
                (true, _) => Text::key(StrId::GroupYou),
                (false, false) => Text::Raw(plain.clone()),
                (false, true) => {
                    Text::Raw(format!("{plain} {}", crate::names::hash_suffix(&m.address)))
                }
            };
            let avatar = Avatar::of(
                chat::initials(Some(plain.as_str()), &m.address),
                &m.address,
                None,
                None,
            );
            let state = Text::key(match super::groups::member_words(m, you) {
                "left" => StrId::GroupStateLeft,
                "admin" => StrId::GroupAdmin,
                "joined" => StrId::GroupStateJoined,
                _ => invited_words(model, info, &m.address),
            });
            let id = NodeId::Member(m.address);
            MemberRow {
                id,
                address: m.address,
                a11y: A11y::of(
                    id,
                    Role::Row,
                    Text::Join(Vec::from([name.clone(), state.clone()])),
                ),
                name,
                state,
                you,
                avatar,
                remove: (info.ours && !you && !m.left).then(|| {
                    verb(Verb::Remove {
                        group: Target::Peer(group),
                        member: Some(m.address),
                        confirmed: false,
                    })
                }),
            }
        })
        .collect();
    let ask = sheet.confirm.map(|confirm| match confirm {
        Confirm::Leave => AskNode {
            words: Text::Key(
                StrId::GroupLeaveAsk,
                Vec::from([Arg::Str(info.name.clone())]),
            ),
            yes: verb(Verb::Leave {
                group: Target::Peer(group),
                confirmed: true,
            }),
            no: verb(Verb::Close),
        },
        Confirm::Remove(member) => AskNode {
            words: Text::Key(
                StrId::GroupRemoveAsk,
                Vec::from([
                    Arg::Str(
                        info.member(&member)
                            .map(|m| model.member_name(m))
                            .unwrap_or_else(|| model.names.label(&member, model.lang)),
                    ),
                    Arg::Str(info.name.clone()),
                ]),
            ),
            yes: verb(Verb::Remove {
                group: Target::Peer(group),
                member: Some(member),
                confirmed: true,
            }),
            no: verb(Verb::Close),
        },
        Confirm::Delete => AskNode {
            words: Text::Key(
                StrId::GroupDeleteAsk,
                Vec::from([Arg::Str(info.name.clone())]),
            ),
            yes: verb(Verb::Delete {
                group: Target::Peer(group),
                confirmed: true,
            }),
            no: verb(Verb::Close),
        },
    });
    let title = Text::Raw(info.name.clone());
    Some(MemberSheet {
        id,
        group,
        a11y: A11y::of(id, Role::Sheet, title.clone()),
        title,
        count: Text::Key(
            StrId::GroupMemberCount,
            Vec::from([Arg::Int(info.count() as i64)]),
        ),
        rows,
        // An ended group has nobody to leave: the same place removes it
        // from the list (1307 pass four), asked as Leave is.
        leave: match info.standing {
            Standing::Joined => Some(verb(Verb::Leave {
                group: Target::Peer(group),
                confirmed: false,
            })),
            Standing::Ended => {
                let mut remove = verb(Verb::Leave {
                    group: Target::Peer(group),
                    confirmed: true,
                });
                remove.label_key = StrId::GroupRemoveFromList;
                Some(remove)
            }
            Standing::Invited | Standing::Left => None,
        },
        add: (info.ours && info.standing == Standing::Joined).then(|| {
            verb(Verb::Add {
                group: Target::Peer(group),
            })
        }),
        delete: (info.ours && info.standing == Standing::Joined).then(|| {
            verb(Verb::Delete {
                group: Target::Peer(group),
                confirmed: false,
            })
        }),
        again: matches!(info.standing, Standing::Left | Standing::Ended).then(|| {
            verb(Verb::Again {
                group: Target::Peer(group),
            })
        }),
        ask,
        close: verb(Verb::Close),
    })
}

/// An invited member's words in the sheet (1307 pass six): where the
/// reader's invitation to it stands, from the home's outbox through the
/// archive. Delivered, it is plainly invited: the member has it and has
/// not joined. In the admin's own group an invitation not seen yet is
/// still being delivered; in another's group, the reader knows only that
/// the member was invited.
fn invited_words<B>(model: &UiModel<B>, info: &GroupInfo, member: &[u8; 16]) -> StrId {
    use leviculum_ui::delivery::DeliveryState as D;
    match model.invitation_delivery(&info.address, member) {
        Some(D::Arrived | D::Delivered | D::Read { .. }) => StrId::GroupStateInvited,
        Some(D::InMailbox) => StrId::GroupStateWaiting,
        Some(D::Failed | D::Rejected) => StrId::GroupStateUndelivered,
        Some(D::Queued | D::Stamping | D::InFlight | D::Sent) => StrId::GroupStateDelivering,
        None if info.ours => StrId::GroupStateDelivering,
        None => StrId::GroupStateInvited,
    }
}

fn hex(bytes: &[u8]) -> Value {
    Value::String(crate::address::to_hex(bytes))
}

fn standing_word(standing: Standing) -> &'static str {
    match standing {
        Standing::Invited => "invited",
        Standing::Joined => "joined",
        Standing::Left => "left",
        Standing::Ended => "ended",
    }
}

impl GroupBadge {
    pub fn to_json(&self) -> Value {
        json!({
            "members": self.members,
            "standing": standing_word(self.standing),
            "words": self.words.to_json(),
        })
    }
}

impl InviteNode {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "group": hex(&self.group),
            "title": self.title.to_json(),
            "trust": self.trust.to_json(),
            "join": self.join.as_ref().map(Action::to_json),
            "state": self.state.as_ref().map(Text::to_json),
            "a11y": self.a11y.to_json(),
        })
    }
}

impl NewGroupNode {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "step": format!("{:?}", self.step),
            "title": self.title.to_json(),
            "rows": self.rows.iter().map(|r| json!({
                "id": r.id.resource(),
                "name": r.name,
                "avatar": leviculum_ui::scene::ToJson::to_json(&r.avatar),
                "picked": r.picked,
                "toggle": r.toggle.to_json(),
                "a11y": r.a11y.to_json(),
            })).collect::<Vec<_>>(),
            "picked": self.picked.to_json(),
            "next": self.next.to_json(),
            "hint": self.hint.to_json(),
            "name": self.name,
            "trust": self.trust.to_json(),
            "create": self.create.to_json(),
            "close": self.close.to_json(),
            "a11y": self.a11y.to_json(),
        })
    }
}

impl MemberSheet {
    pub fn to_json(&self) -> Value {
        let mut value = self.json();
        // The admin's Add members (G2) only where it is, so the sheets of
        // a member stay the bytes they were.
        if let (Value::Object(map), Some(add)) = (&mut value, &self.add) {
            map.insert("add".into(), add.to_json());
        }
        // Delete and Start again (pass four) likewise.
        if let (Value::Object(map), Some(delete)) = (&mut value, &self.delete) {
            map.insert("delete".into(), delete.to_json());
        }
        if let (Value::Object(map), Some(again)) = (&mut value, &self.again) {
            map.insert("again".into(), again.to_json());
        }
        value
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "group": hex(&self.group),
            "title": self.title.to_json(),
            "count": self.count.to_json(),
            "rows": self.rows.iter().map(|r| json!({
                "id": r.id.resource(),
                "name": r.name.to_json(),
                "state": r.state.to_json(),
                "you": r.you,
                "avatar": r.avatar.to_json(),
                "remove": r.remove.as_ref().map(Action::to_json),
                "a11y": r.a11y.to_json(),
            })).collect::<Vec<_>>(),
            "leave": self.leave.as_ref().map(Action::to_json),
            "ask": self.ask.as_ref().map(|a| json!({
                "words": a.words.to_json(),
                "yes": a.yes.to_json(),
                "no": a.no.to_json(),
            })),
            "close": self.close.to_json(),
            "actions": actions(&self.rows.iter().filter_map(|r| r.remove.clone()).collect::<Vec<_>>()),
            "a11y": self.a11y.to_json(),
        })
    }
}
