//! The Rooms (1379, #232): Reticulum Relay Chat beside the groups, not
//! among them. A room lives on a hub (`rrc.hub`), is a name, and is live:
//! no mailbox keeps it, a line nobody was there for is gone, and there
//! are no receipts and no ticks. So a room is no conversation: it has its
//! own tab, its own list and its own lines, and when one is opened the
//! surface says in one line what a room is not.
//!
//! The model knows the hub the config names, how the link to it stands,
//! the rooms the hub says we are in, and what was said in them while we
//! were there.
//!
//! A room is drawn as a chat (1450, walk 1 finding 20): bubbles, the
//! writer's avatar and name over the first of a run, the comings and
//! goings as quiet lines in the middle, the people seen in it under the
//! room's name and their list behind it, and "Leave room" in its menu.
//! The input is a compose field: in a room a line goes to the room; a
//! room name (`#lab`) joins one. IRC's commands stay as shortcuts for
//! those who know them (`/me` makes an action, `/part` leaves, `/join
//! #lab key` joins a room with a key), and nothing on screen names them.
//! What reaches the hub is the shell's ([`UiEffect::Rooms`],
//! `crate::rooms_link`), what comes back is [`RoomsEvent`].

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_rrc::{LineKind, Refusal};
use leviculum_ui::input::InputBuffer;
use leviculum_ui::toast::ToastKind;
use serde_json::{json, Value};

use super::chat;
use super::scene::{Avatar, TabId};
use super::strings::{Arg, StrId, Text};
use super::{UiEffect, UiModel};
use crate::when::WallTime;

/// How many lines a room keeps while the app runs: a room is live, and
/// what scrolled past this is gone, as it is for the hub.
pub const LINES_KEPT: usize = 500;

/// The body limit before the hub said its own (rrcd's, and the spec's).
pub const BODY_LIMIT: usize = 350;

/// What the shell is asked to do with the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomAsk {
    /// Link to the hub at this `rrc.hub` destination, leaving any other.
    Open {
        hub: [u8; 16],
    },
    Join {
        room: String,
        key: Option<String>,
    },
    Part {
        room: String,
    },
    Say {
        room: String,
        kind: LineKind,
        text: String,
    },
    /// Leave the hub.
    Close,
    /// Ask this hub which rooms it names (1621): a link of its own beside
    /// the rooms' link, identified with a throwaway identity and no nick,
    /// HELLO until WELCOME, the notices of a few seconds read for rooms
    /// ([`super::hubs::rooms_named`]), then closed. It joins nothing and
    /// the hub learns nothing of the reader: a server's sheet asks it on
    /// opening, which the privacy sentence of 1577 allows because nothing
    /// that names the reader goes out. The answer is [`RoomsEvent::Peeked`].
    Peek {
        hub: [u8; 16],
    },
}

/// What a hub named when its sheet asked (1621), kept for the run so the
/// next opening shows it at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// The question is on its way, asked at `since` (unix seconds, the
    /// model's clock; `None` before the clock came). The sheet waits
    /// [`ASK_PATIENCE_SECS`] before it says the server does not answer.
    Asking { since: Option<i64> },
    /// The rooms its notices named, maybe none.
    Rooms(Vec<String>),
    /// It gave no WELCOME (no path, no link, no answer).
    NoAnswer,
}

/// How long a server's sheet asks, and the rooms screen connects, before
/// either says the server does not answer and offers to try again (1653,
/// walk 8, 42: the sentence stood 90 s, the question's whole budget, and
/// the reader waited without end). One patience for both since 1659 (walk
/// 10, 53: the sheet gave up at 20 s, the screen at 30 s, and Minuteman
/// answered after 45 s). The question itself runs on to its budget; an
/// answer that comes later still fills the sheet, and a welcome that
/// comes later still opens the screen.
pub const ASK_PATIENCE_SECS: i64 = 45;

/// From this second on the sheet and the rooms screen say the server is
/// slow, with the seconds (1659, walk 10, 53), until [`ASK_PATIENCE_SECS`].
pub const SLOW_SECS: i64 = 20;

/// The settings' hub row (1577): what a press on one of its lines asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubPick {
    /// No public rooms.
    None,
    /// This hub: one heard, or one of [`super::hubs::shipped`].
    Hub([u8; 16]),
    /// "More…" (1599): the next page of hubs in the terminal; the window
    /// and the phone show them all in their scroll.
    More,
    /// "Another hub: paste its address": the terminal asks for it in the
    /// palette, the window has its field; the phone sends the text
    /// itself ([`HubChoice::Address`]).
    Paste,
}

/// The hub the reader chose (1577), as the model takes it: none, a hash,
/// or the text pasted, which the model reads as an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubChoice {
    None,
    Hub([u8; 16]),
    Address(String),
}

/// Why the link to the hub is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Down {
    /// No path to the hub, or no announce to verify it by.
    NoPath,
    /// The link did not come up, or the hub never welcomed us.
    NoAnswer,
    /// The link closed under us.
    Lost,
    /// The node the messenger lent is gone.
    NodeGone,
    /// The reader left.
    Left,
}

/// How the link to the hub stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubState {
    /// Not linked, never asked.
    Off,
    Linking,
    Ready,
    Down(Down),
}

/// What a line in a room is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomLineKind {
    /// A member's line, a NOTICE or an ACTION.
    Said(LineKind),
    /// The hub's own notice.
    Hub,
    /// Someone came in or left (`None` in the line's nick: we did).
    Joined,
    Parted,
    /// The hub refused something.
    Error,
}

/// One line of a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomLine {
    pub kind: RoomLineKind,
    pub nick: Option<String>,
    pub text: String,
    /// The hub's echo of a line we sent.
    pub ours: bool,
    /// Milliseconds since the epoch, as the envelope says.
    pub ts_ms: u64,
}

/// A room we are in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Room {
    pub name: String,
    pub lines: Vec<RoomLine>,
    /// Lines that came while it was not open.
    pub unread: u32,
    /// The others seen in it while we were there, in the order they came:
    /// who came in or wrote, and has not left. The hub sends no list of
    /// members, so this is what the room's header and its people say.
    pub members: Vec<String>,
}

impl Room {
    fn push(&mut self, line: RoomLine) {
        if self.lines.len() >= LINES_KEPT {
            self.lines.remove(0);
        }
        if !line.ours {
            if let Some(nick) = &line.nick {
                match line.kind {
                    RoomLineKind::Parted => self.members.retain(|m| m != nick),
                    RoomLineKind::Joined | RoomLineKind::Said(_) => {
                        if !self.members.contains(nick) {
                            self.members.push(nick.clone());
                        }
                    }
                    RoomLineKind::Hub | RoomLineKind::Error => {}
                }
            }
        }
        self.lines.push(line);
    }
}

/// The Rooms tab's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rooms {
    /// The host shows a Rooms tab: the config names a hub.
    pub enabled: bool,
    /// The tab is in front.
    pub open: bool,
    /// The hub the config names.
    pub hub: Option<[u8; 16]>,
    /// The name the hub gave in its WELCOME.
    pub hub_name: Option<String>,
    pub state: HubState,
    pub body_limit: usize,
    pub rooms: Vec<Room>,
    /// The open room, an index into `rooms`.
    pub selected: Option<usize>,
    /// The open room's people are shown, behind its header.
    pub people: bool,
    /// The input line.
    pub input: String,
    /// The hub the reader was last told, before the first link to it,
    /// that their name is visible to everyone in a room (1577): the
    /// config's `rooms_told`.
    pub told: Option<[u8; 16]>,
    /// The hubs heard announcing (1599), from this device's node and from
    /// its home: what the settings offer first.
    pub heard: super::hubs::Heard,
    /// The page of hubs the settings show (1599): the first
    /// [`super::settings::HUB_PAGE`] hubs on 0, "More…" turns it.
    pub hub_page: usize,
    /// Joins asked before the hub answered (1578): the first join links,
    /// and the hub's welcome sends them.
    pub queued: Vec<RoomAsk>,
    /// What each hub named when its sheet asked (1621).
    pub named: alloc::collections::BTreeMap<[u8; 16], Named>,
    /// What each hub named when the mailbox asked it (1643,
    /// `crate::home::hub_rooms`), pulled with the hub: the sheet lists it
    /// at once, until this device's own question answers ([`Self::named`]
    /// with rooms), which is the fresher copy.
    pub mailbox_named: alloc::collections::BTreeMap<[u8; 16], Vec<String>>,
    /// The chosen hub's answer to `/list` after its WELCOME (1652): `None`
    /// until it came, then the rooms it names, maybe none.
    pub listed: Option<Vec<String>>,
    /// When the link was asked for, in seconds since the epoch, while it
    /// is coming up: "Connecting… 12 s" after the first five (1652).
    pub linking_since: Option<i64>,
}

impl Default for Rooms {
    fn default() -> Self {
        Rooms {
            enabled: false,
            open: false,
            hub: None,
            hub_name: None,
            state: HubState::Off,
            body_limit: BODY_LIMIT,
            rooms: Vec::new(),
            selected: None,
            people: false,
            input: String::new(),
            told: None,
            heard: super::hubs::Heard::default(),
            hub_page: 0,
            queued: Vec::new(),
            named: alloc::collections::BTreeMap::new(),
            mailbox_named: alloc::collections::BTreeMap::new(),
            listed: None,
            linking_since: None,
        }
    }
}

impl Rooms {
    /// The tab is on this host and in front: its input has the keys.
    pub fn showing(&self) -> bool {
        self.enabled && self.open
    }

    /// The open room.
    pub fn room(&self) -> Option<&Room> {
        self.selected.and_then(|i| self.rooms.get(i))
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.rooms.iter().position(|r| r.name == name)
    }

    fn room_mut(&mut self, name: &str) -> &mut Room {
        let index = match self.index(name) {
            Some(index) => index,
            None => {
                self.rooms.push(Room {
                    name: name.to_string(),
                    ..Room::default()
                });
                self.rooms.len() - 1
            }
        };
        &mut self.rooms[index]
    }

    /// A line for `room`; unread unless it is the open one or ours.
    fn line(&mut self, room: &str, line: RoomLine) {
        let open = self.open && self.room().is_some_and(|r| r.name == room);
        let ours = line.ours;
        let target = self.room_mut(room);
        if !open && !ours {
            target.unread = target.unread.saturating_add(1);
        }
        target.push(line);
    }
}

/// What the Rooms are told: what the hub said, and the reader's verbs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomsEvent {
    /// The shell is linking to the hub.
    Linking,
    /// The hub welcomed us, by this name, taking bodies of this size.
    Welcomed {
        name: Option<String>,
        body_limit: usize,
    },
    Joined {
        room: String,
        nick: Option<String>,
    },
    Parted {
        room: String,
        nick: Option<String>,
    },
    Line {
        room: String,
        kind: LineKind,
        text: String,
        nick: Option<String>,
        ours: bool,
        ts_ms: u64,
    },
    HubNotice {
        room: Option<String>,
        text: String,
    },
    Error {
        room: Option<String>,
        text: String,
    },
    /// What the reader asked was not sent.
    Refused(Refusal),
    Down(Down),
    /// The reader opens this room (an index into the list), or the list
    /// with `None`.
    Show(Option<usize>),
    /// The input line holds this now (a platform's own text field).
    Typed(String),
    /// The input line is sent.
    Submit,
    /// The open room's header: its people are shown, or hidden again.
    People(bool),
    /// The open room's menu: "Leave room".
    Leave,
    /// The settings chose the public rooms' hub (1577).
    Hub(HubChoice),
    /// A hub was heard announcing (1599): by this device's node, or by its
    /// home, which remembers it for every device.
    Heard(super::hubs::HeardHub),
    /// "More…" under the hubs in the settings (1599, [`more`]).
    More,
    /// The answer to [`RoomAsk::Peek`] (1621): the rooms the hub's notices
    /// named, or `None` when it never welcomed the question.
    Peeked {
        hub: [u8; 16],
        rooms: Option<Vec<String>>,
    },
    /// The chosen hub's answer to `/list` after its WELCOME (1652), merged
    /// with what its greeting named; empty when it names none, said an
    /// ERROR, or said nothing in time.
    Listed {
        rooms: Vec<String>,
    },
    /// A tap on one of the rooms the hub names (1652): joined as the hub
    /// spells it, and opened once the hub says we are in.
    Join(String),
    /// "Try again" or "Connect" under the link's state (1652).
    Retry,
}

/// A tab chosen: the Rooms come to the front, any other sends them back.
/// Since 1652 the hub was chosen by the reader, told then that their name
/// is visible ([`link`], 1577's sentence), so opening the Rooms links to
/// it and asks for its rooms: a tab that waits for a typed room name was
/// the "Connecting…" that never connected.
pub(super) fn tab<B>(model: &mut UiModel<B>, tab: TabId) -> Vec<UiEffect> {
    match tab {
        TabId::Rooms if model.rooms.enabled => {
            model.rooms.open = true;
            if let Some(room) = model
                .rooms
                .selected
                .and_then(|i| model.rooms.rooms.get_mut(i))
            {
                room.unread = 0;
            }
            link(model)
        }
        TabId::Rooms => Vec::new(),
        TabId::Chats | TabId::Map | TabId::Browse | TabId::Explore => {
            model.rooms.open = false;
            Vec::new()
        }
    }
}

/// Link to the hub unless linked or linking; a link coming longer than
/// [`ASK_PATIENCE_SECS`] is asked for again (1659, the "Try again" the
/// screen offers then).
fn link<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let Some(hub) = model.rooms.hub else {
        return Vec::new();
    };
    let again = overdue(&model.rooms, model.now);
    match model.rooms.state {
        HubState::Linking if !again => Vec::new(),
        HubState::Ready => Vec::new(),
        HubState::Linking | HubState::Off | HubState::Down(_) => {
            model.rooms.state = HubState::Linking;
            model.rooms.linking_since = model.now.map(|now| now.unix_secs);
            let mut effects = Vec::new();
            // Before the first link to a hub, once (1577): the hub learns
            // the reader's identity when the link identifies, and the
            // rooms show their name.
            if model.rooms.told != Some(hub) {
                model.rooms.told = Some(hub);
                let text = name_notice(model).resolve(model.lang);
                model.toast(ToastKind::Info, text);
                effects.push(UiEffect::RememberHash {
                    key: "rooms_told",
                    hash: Some(hub),
                });
            }
            effects.push(UiEffect::Rooms(RoomAsk::Open { hub }));
            effects
        }
    }
}

/// The hub the settings chose (1577): the link to the old one closes, the
/// rooms of the old one go, the tab is there with a hub and gone without,
/// and the config keeps the choice. An address that is none is said, and
/// nothing changes.
/// What a room shows of the reader, said before they go in (1629, walk 3
/// new 9: "your name is visible" beside a profile with no name): the
/// name where there is one; without one the hub link goes without a nick
/// and a room shows the writer with no name ([`line_node`]).
///
/// Walk 11, finding 62 (1667): "you have no name here" beside a profile
/// the mailbox page calls "lew" read as if the profile had none. The
/// notice says it is this server that has no name for the reader, and
/// names the profile's where the mailbox or the reader's own announce
/// carries one.
pub(super) fn name_notice<B>(model: &UiModel<B>) -> Text {
    if !model.profile.name.trim().is_empty() {
        return Text::key(StrId::RoomsNameVisible);
    }
    match profile_name_elsewhere(model) {
        Some(name) => Text::Key(StrId::RoomsNoNameProfile, Vec::from([Arg::Str(name)])),
        None => Text::key(StrId::RoomsNoName),
    }
}

/// The reader's name as the mailbox says it, else as the reader's own
/// announce carries it: the words the mailbox page shows for "Name".
fn profile_name_elsewhere<B>(model: &UiModel<B>) -> Option<String> {
    let told = match &model.profile.told {
        super::mailbox_page::Told::Told(about) => Some(about.name.trim()),
        _ => None,
    };
    let away = match &model.profile.mailbox {
        super::profile::Mailbox::Away { name, .. } => name.as_deref().map(str::trim),
        _ => None,
    };
    let announced = model
        .address
        .and_then(|own| model.names.announced(&own))
        .map(str::trim);
    [told, away, announced]
        .into_iter()
        .flatten()
        .find(|n| !n.is_empty())
        .map(String::from)
}

pub(super) fn choose<B>(model: &mut UiModel<B>, choice: HubChoice) -> Vec<UiEffect> {
    let hub = match choice {
        HubChoice::None => None,
        HubChoice::Hub(hub) => Some(hub),
        HubChoice::Address(text) => match crate::address::parse(&text) {
            Ok(hub) => Some(hub),
            Err(_) => {
                let text = Text::key(StrId::RoomsHubBad).resolve(model.lang);
                model.toast(ToastKind::Error, text);
                return Vec::new();
            }
        },
    };
    if model.rooms.hub == hub {
        return Vec::new();
    }
    let mut effects = Vec::new();
    if matches!(model.rooms.state, HubState::Linking | HubState::Ready) {
        effects.push(UiEffect::Rooms(RoomAsk::Close));
    }
    let told = model.rooms.told;
    // The hubs heard stay (1578): Explore lists them beside the one chosen,
    // and choosing one is no reason to forget the others.
    let heard = core::mem::take(&mut model.rooms.heard);
    model.rooms = Rooms {
        enabled: hub.is_some(),
        hub,
        told,
        heard,
        ..Rooms::default()
    };
    super::settings::settle_suggestion(&mut model.rooms);
    effects.push(UiEffect::RememberHash {
        key: "rooms_hub",
        hash: hub,
    });
    let said = match hub {
        Some(_) => Text::Key(
            StrId::RoomsHubChosen,
            Vec::from([Arg::Text(alloc::boxed::Box::new(hub_label(&model.rooms)))]),
        ),
        None => Text::key(StrId::RoomsHubOff),
    };
    let said = said.resolve(model.lang);
    model.toast(ToastKind::Info, said);
    // Choosing a server connects to it (1652): path, link, HELLO, WELCOME,
    // then its rooms. The privacy sentence comes with it, once a hub.
    effects.extend(link(model));
    effects
}

/// The chosen hub in the reader's words (1577): a shipped hub's name, the
/// name the hub gave in its welcome, or "Your own hub"; "None" without
/// one. Never its hash.
pub fn hub_label(rooms: &Rooms) -> Text {
    let Some(hub) = rooms.hub else {
        return Text::key(StrId::RoomsHubNone);
    };
    if let Some(shipped) = super::hubs::named(&hub) {
        return Text::Raw(shipped.name);
    }
    if let Some(name) = rooms.heard.get(&hub).and_then(|h| h.name.clone()) {
        return Text::Raw(name);
    }
    match &rooms.hub_name {
        Some(name) if !name.trim().is_empty() => Text::Raw(name.clone()),
        _ => Text::key(StrId::RoomsHubOwn),
    }
}

/// "More…" under the hubs (1599): the next page, back to the first after
/// the last; a window and a phone show them all from the second on
/// ([`super::settings::hub_rows`]).
pub fn more<B>(model: &mut UiModel<B>) {
    let hubs = super::settings::offered_hubs(&model.rooms).len();
    let pages = hubs.div_ceil(super::settings::HUB_PAGE).max(1);
    model.rooms.hub_page = (model.rooms.hub_page + 1) % pages;
}

/// The palette's `rooms` and the terminal's `R`: the tab where the host
/// has one, and where it has none the reader is told how to get it.
pub(super) fn open<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    if model.rooms.enabled {
        model.browse.open = false;
        return tab(model, TabId::Rooms);
    }
    let text = Text::key(StrId::RoomsOff).resolve(model.lang);
    model.toast(ToastKind::Info, text);
    Vec::new()
}

/// Fold one of the Rooms' events.
pub(super) fn fold<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    event: RoomsEvent,
) -> Vec<UiEffect> {
    let effects = fold_event(model, event);
    // A hub heard, answered or failed moves what is suggested (1629).
    super::settings::settle_suggestion(&mut model.rooms);
    effects
}

fn fold_event<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    event: RoomsEvent,
) -> Vec<UiEffect> {
    let rooms = &mut model.rooms;
    match event {
        RoomsEvent::Linking => {
            if rooms.state != HubState::Linking {
                rooms.linking_since = model.now.map(|now| now.unix_secs);
            }
            rooms.state = HubState::Linking;
        }
        RoomsEvent::Welcomed { name, body_limit } => {
            rooms.state = HubState::Ready;
            rooms.linking_since = None;
            rooms.listed = None;
            if let Some(hub) = rooms.hub {
                rooms.heard.answered(&hub, true);
            }
            rooms.hub_name = name;
            rooms.body_limit = body_limit;
            // A link that came back joins again what it was in, and the
            // first link the joins that asked for it (1578).
            let mut again: Vec<UiEffect> = rooms
                .rooms
                .iter()
                .map(|r| {
                    UiEffect::Rooms(RoomAsk::Join {
                        room: r.name.clone(),
                        key: None,
                    })
                })
                .collect();
            again.extend(rooms.queued.drain(..).map(UiEffect::Rooms));
            return again;
        }
        RoomsEvent::Joined { room, nick } => {
            let us = nick.is_none();
            let line = RoomLine {
                kind: RoomLineKind::Joined,
                nick,
                text: String::new(),
                ours: us,
                ts_ms: 0,
            };
            rooms.line(&room, line);
            if us {
                rooms.selected = rooms.index(&room);
            }
        }
        RoomsEvent::Parted { room, nick } => {
            if nick.is_none() {
                if let Some(index) = rooms.index(&room) {
                    rooms.rooms.remove(index);
                    rooms.selected = None;
                    rooms.people = false;
                }
            } else {
                let line = RoomLine {
                    kind: RoomLineKind::Parted,
                    nick,
                    text: String::new(),
                    ours: false,
                    ts_ms: 0,
                };
                rooms.line(&room, line);
            }
        }
        RoomsEvent::Line {
            room,
            kind,
            text,
            nick,
            ours,
            ts_ms,
        } => rooms.line(
            &room,
            RoomLine {
                kind: RoomLineKind::Said(kind),
                nick,
                text,
                ours,
                ts_ms,
            },
        ),
        RoomsEvent::HubNotice { room, text } | RoomsEvent::Error { room, text }
            if room.as_deref().is_some_and(|r| rooms.index(r).is_some()) =>
        {
            let kind = match &rooms.state {
                _ if text.is_empty() => return Vec::new(),
                _ => RoomLineKind::Hub,
            };
            let room = room.unwrap_or_default();
            rooms.line(
                &room,
                RoomLine {
                    kind,
                    nick: None,
                    text,
                    ours: false,
                    ts_ms: 0,
                },
            );
        }
        RoomsEvent::HubNotice { text, .. } => model.toast(ToastKind::Info, text),
        RoomsEvent::Error { text, .. } => model.toast(ToastKind::Error, text),
        RoomsEvent::Refused(refusal) => {
            let text = refused(&refusal).resolve(model.lang);
            model.toast(ToastKind::Error, text);
        }
        RoomsEvent::Down(why) => {
            rooms.state = HubState::Down(why);
            rooms.linking_since = None;
            // A hub heard but not answering stays listed, marked (1599).
            if let (Some(hub), Down::NoAnswer | Down::NoPath) = (rooms.hub, why) {
                rooms.heard.answered(&hub, false);
            }
            let text = down(why).resolve(model.lang);
            // The rooms screen in front says it in its state line; a toast
            // as well stood doubled under it (1659, walk 10, 55).
            if why != Down::Left && !rooms.open {
                model.toast(ToastKind::Error, text);
            }
        }
        RoomsEvent::Show(index) => {
            rooms.selected = index.filter(|i| *i < rooms.rooms.len());
            rooms.people = false;
            if let Some(room) = rooms.selected.and_then(|i| rooms.rooms.get_mut(i)) {
                room.unread = 0;
            }
        }
        RoomsEvent::Typed(text) => rooms.input = text,
        RoomsEvent::Submit => return submit(model),
        RoomsEvent::People(shown) => rooms.people = shown && rooms.room().is_some(),
        RoomsEvent::Leave => {
            let Some(room) = rooms.room().map(|r| r.name.clone()) else {
                return Vec::new();
            };
            return ask(model, RoomAsk::Part { room });
        }
        RoomsEvent::Hub(choice) => return choose(model, choice),
        RoomsEvent::Heard(hub) => {
            rooms.heard.hear(hub);
        }
        RoomsEvent::More => more(model),
        RoomsEvent::Peeked { hub, rooms: named } => {
            let named = match named {
                Some(named) => Named::Rooms(named),
                None => Named::NoAnswer,
            };
            rooms.named.insert(hub, named);
        }
        RoomsEvent::Listed { rooms: listed } => {
            if let Some(hub) = rooms.hub {
                rooms.named.insert(hub, Named::Rooms(listed.clone()));
            }
            rooms.listed = Some(listed);
        }
        RoomsEvent::Join(room) => {
            return ask(model, RoomAsk::Join { room, key: None });
        }
        RoomsEvent::Retry => return link(model),
    }
    Vec::new()
}

/// A key while the Rooms tab is in front (the terminal; a phone and a
/// window have their own field and say [`RoomsEvent::Typed`]): letters
/// go to the input line, `Backspace` takes one back, `Enter` sends it,
/// `Up` and `Down` step the rooms, `Esc` closes the people, then the open
/// room and, with none open, leaves the tab for the chats, as `Ctrl-g`
/// does. `Alt-p` shows the open room's people, `Alt-l` leaves it: the
/// header and the menu the phone and the window tap (no `Ctrl` chord:
/// `Ctrl-l` redraws everywhere, `Ctrl-p` and `Ctrl-w` edit a line).
pub(super) fn key<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    key: leviculum_ui::input::KeyEvent,
) -> Vec<UiEffect> {
    use leviculum_ui::input::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let rooms = &mut model.rooms;
    match key.code {
        KeyCode::Char('g') if ctrl => return leave(model),
        KeyCode::Char('p') if alt => {
            let shown = !rooms.people;
            return fold(model, RoomsEvent::People(shown));
        }
        KeyCode::Char('l') if alt => return fold(model, RoomsEvent::Leave),
        KeyCode::Char(c) if !ctrl && !alt => {
            rooms.input.push(c);
        }
        KeyCode::Backspace => {
            rooms.input.pop();
        }
        KeyCode::Enter => return submit(model),
        KeyCode::Up | KeyCode::Down if !rooms.rooms.is_empty() => {
            let last = rooms.rooms.len() - 1;
            let next = match (rooms.selected, key.code) {
                (None, KeyCode::Up) => last,
                (None, _) => 0,
                (Some(i), KeyCode::Up) => i.saturating_sub(1),
                (Some(i), _) => (i + 1).min(last),
            };
            return fold(model, RoomsEvent::Show(Some(next)));
        }
        KeyCode::Esc if rooms.people => rooms.people = false,
        KeyCode::Esc if rooms.selected.is_some() => rooms.selected = None,
        KeyCode::Esc => return leave(model),
        _ => {}
    }
    Vec::new()
}

/// Leave the tab for the chats.
fn leave<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    model.rooms.open = false;
    Vec::new()
}

/// The input line sent: a command, a room to join, or a line for the open
/// room. A line the hub would not take stays in the input, and a toast
/// says why, before anything is sent.
fn submit<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let input = model.rooms.input.trim().to_string();
    if input.is_empty() {
        // The terminal's Enter on an empty field: the "Connect" or "Try
        // again" under the link's state (1652).
        return match model.rooms.state {
            HubState::Off | HubState::Down(_) if model.rooms.room().is_none() => link(model),
            _ => Vec::new(),
        };
    }
    let open = model.rooms.room().map(|r| r.name.clone());
    // A room the hub names, typed as it is listed (1652): the terminal's
    // tap on its row, a space in its name or not.
    let listed = match &open {
        None => listed(&model.rooms)
            .and_then(|l| l.rooms.into_iter().find(|r| r.eq_ignore_ascii_case(&input))),
        Some(_) => None,
    };
    let ask = if let Some(room) = listed {
        Some(RoomAsk::Join { room, key: None })
    } else if let Some(rest) = input.strip_prefix("/join ") {
        let mut words = rest.split_whitespace();
        words.next().map(|room| RoomAsk::Join {
            room: room.to_string(),
            key: words.next().map(ToString::to_string),
        })
    } else if input == "/part" || input.starts_with("/part ") {
        input[5..]
            .split_whitespace()
            .next()
            .map(ToString::to_string)
            .or(open)
            .map(|room| RoomAsk::Part { room })
    } else if open.is_none() && input.starts_with('#') && !input.contains(' ') {
        Some(RoomAsk::Join {
            room: input.clone(),
            key: None,
        })
    } else if let Some(room) = open {
        let (kind, text) = match input.strip_prefix("/me ") {
            Some(action) => (LineKind::Action, action.to_string()),
            None => (LineKind::Msg, input.clone()),
        };
        if text.len() > model.rooms.body_limit {
            let refusal = Refusal::TooLong {
                bytes: text.len(),
                limit: model.rooms.body_limit,
            };
            let said = refused(&refusal).resolve(model.lang);
            model.toast(ToastKind::Error, said);
            return Vec::new();
        }
        Some(RoomAsk::Say { room, kind, text })
    } else {
        None
    };
    let Some(asked) = ask else {
        let text = Text::key(StrId::RoomsHowToJoin).resolve(model.lang);
        model.toast(ToastKind::Info, text);
        return Vec::new();
    };
    let effects = self::ask(model, asked.clone());
    if !effects.is_empty() || model.rooms.queued.contains(&asked) {
        model.rooms.input.clear();
    }
    effects
}

/// `asked` to the hub, or, before it answered, a toast that says so. A
/// join before the hub answered links to it and waits for its welcome
/// (1578): the first join is what opens the link.
fn ask<B>(model: &mut UiModel<B>, asked: RoomAsk) -> Vec<UiEffect> {
    if model.rooms.state != HubState::Ready
        && model.rooms.hub.is_some()
        && matches!(asked, RoomAsk::Join { .. })
    {
        model.rooms.queued.retain(|q| q != &asked);
        model.rooms.queued.push(asked);
        return link(model);
    }
    if model.rooms.state != HubState::Ready {
        let text = refused(&Refusal::NotReady).resolve(model.lang);
        model.toast(ToastKind::Error, text);
        return Vec::new();
    }
    Vec::from([UiEffect::Rooms(asked)])
}

/// A refusal in the reader's words.
pub fn refused(refusal: &Refusal) -> Text {
    let int = |n: usize| Arg::Int(i64::try_from(n).unwrap_or(i64::MAX));
    match refusal {
        Refusal::NotReady => Text::key(StrId::RoomsNotReady),
        Refusal::NotInRoom(room) => {
            Text::Key(StrId::RoomsNotIn, Vec::from([Arg::Str(room.clone())]))
        }
        Refusal::Empty => Text::key(StrId::RoomsHowToJoin),
        Refusal::TooLong { bytes, limit } => {
            Text::Key(StrId::RoomsTooLong, Vec::from([int(*bytes), int(*limit)]))
        }
        Refusal::TooBig { bytes, mdu } => {
            Text::Key(StrId::RoomsTooBig, Vec::from([int(*bytes), int(*mdu)]))
        }
        Refusal::BadRoom => Text::key(StrId::RoomsBadRoom),
        Refusal::TooManyRooms(n) => Text::Key(StrId::RoomsTooMany, Vec::from([int(*n)])),
    }
}

/// Why the hub is down, in the reader's words.
pub fn down(why: Down) -> Text {
    Text::key(match why {
        Down::NoPath => StrId::RoomsNoPath,
        Down::NoAnswer => StrId::RoomsNoAnswer,
        Down::Lost => StrId::RoomsLost,
        Down::NodeGone => StrId::RoomsLost,
        Down::Left => StrId::RoomsLeft,
    })
}

/// The Rooms surface as the frontends draw it (1450): the rooms beside
/// the open one, drawn as a chat, and the compose field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomsNode {
    /// The screen's title (1659, walk 10, 52): "Rooms on EuskalReticulum"
    /// with no room open, "Rooms" without a server or with a room open,
    /// whose header names it.
    pub title: Text,
    /// How the link stands while it is not up ("Connecting…", why it is
    /// down); none while it is: a working link says nothing.
    pub status: Option<Text>,
    pub rows: Vec<RoomRow>,
    pub open: Option<RoomView>,
    /// What the field holds.
    pub input: String,
    /// The field's own hint, the one place it is said: "Message" in a
    /// room, "Room name, like #lab" with none open.
    pub placeholder: Text,
    /// What stands where the open room would, with none open: how to
    /// join one.
    pub empty: Option<Text>,
    /// The verb under the state while the link is not up and not coming
    /// (1652): "Connect" before the first link, "Try again" after a
    /// failure; a tap is [`RoomsEvent::Retry`].
    pub retry: Option<Text>,
    /// The rooms the hub names, with no room open (1652).
    pub listed: Option<ListedNode>,
}

/// The rooms a hub names (1652), over the typed-name field: a tap joins
/// one ([`RoomsEvent::Join`]) and opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedNode {
    /// "Rooms on ShadowLink".
    pub title: Text,
    /// The rooms it names that we are not in, as the hub spells them.
    pub rooms: Vec<String>,
    /// While it is asked, or when it names none.
    pub note: Option<Text>,
    /// Over the typed-name field, below the rooms: "Join another room";
    /// none when the hub names none and the field is the way in.
    pub other: Option<Text>,
}

/// How many seconds of "Connecting…" go by before they are counted.
pub const LINKING_QUIET_SECS: i64 = 5;

/// A room in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRow {
    pub name: String,
    pub unread: u32,
    pub selected: bool,
    /// The room's round avatar, its first letter, as a chat's.
    pub avatar: Avatar,
    /// The word beside the name with the globe mark (1577): "public". A
    /// room is open to everyone on its hub, unlike a chat or a group.
    pub public: Text,
}

/// The mark drawn before [`RoomRow::public`] where a glyph can be.
pub const PUBLIC_MARK: &str = "\u{1F310}";

/// A verb of the open room's menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomVerb {
    pub label: Text,
    /// What a tap on it tells the model.
    pub event: RoomsEvent,
}

/// The open room, as a chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomView {
    pub name: String,
    pub avatar: Avatar,
    /// Under the name, as a group's header says its members: "bob,
    /// carol, you", or "Only you so far". A tap on the header shows
    /// [`RoomView::people`].
    pub subtitle: Text,
    /// The people seen here, the reader last, while the header was
    /// tapped ([`RoomsEvent::People`]); none otherwise.
    pub people: Option<Vec<Text>>,
    /// The line over the people: who the list holds.
    pub people_note: Text,
    /// The room's menu: its people, and "Leave room".
    pub menu: Vec<RoomVerb>,
    /// "A room is live: …", the one line at the top of every room, as a
    /// chat's notice in the middle.
    pub live: Text,
    pub lines: Vec<RoomLineNode>,
}

/// One drawn line of a room: a bubble, or a quiet line in the middle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomLineNode {
    /// The nick, or the reader's own word ("you"), empty for the hub.
    pub who: Text,
    pub text: Text,
    pub kind: RoomLineKind,
    pub ours: bool,
    /// A line someone said, drawn as a bubble (ours on the right); the
    /// rest, comings and goings and the hub's words, as a quiet line in
    /// the middle.
    pub bubble: bool,
    /// The first bubble of another's run: the writer's name over it and
    /// the avatar beside it, as in a group's chat.
    pub named: bool,
    /// The writer's avatar, on the first bubble of another's run.
    pub avatar: Option<Avatar>,
    /// `14:05`, when the hub stamped the line.
    pub time: Option<String>,
}

/// A nick's stand-in for a peer's hash: what picks its avatar's colour,
/// the same for the same nick everywhere.
fn nick_key(nick: &str) -> [u8; 16] {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in nick.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&hash.to_le_bytes());
    key[8..].copy_from_slice(&hash.rotate_left(29).to_le_bytes());
    key
}

/// The round avatar of a nick or a room's name: its first letter.
pub fn avatar(name: &str) -> Avatar {
    let key = nick_key(name);
    Avatar::of(chat::initials(Some(name), &key), &key, None, None)
}

/// The Rooms surface for the frontends.
pub fn node<B>(model: &UiModel<B>) -> RoomsNode {
    let rooms = &model.rooms;
    let status = match rooms.state {
        _ if rooms.hub.is_none() => Some(Text::key(StrId::RoomsNoHub)),
        HubState::Ready => None,
        _ => link_words(model),
    };
    let retry = match rooms.state {
        _ if rooms.hub.is_none() => None,
        HubState::Off | HubState::Down(Down::Left) => Some(Text::key(StrId::RoomsConnect)),
        HubState::Down(_) => Some(Text::key(StrId::RoomsRetry)),
        HubState::Linking if overdue(rooms, model.now) => Some(Text::key(StrId::RoomsRetry)),
        HubState::Linking | HubState::Ready => None,
    };
    let rows = rooms
        .rooms
        .iter()
        .enumerate()
        .map(|(i, r)| RoomRow {
            name: r.name.clone(),
            unread: r.unread,
            selected: rooms.selected == Some(i),
            avatar: avatar(&r.name),
            public: Text::key(StrId::RoomsPublic),
        })
        .collect();
    let offset = model.now.map_or(0, |now| now.offset_secs);
    let open = rooms.room().map(|room| view(room, rooms.people, offset));
    let listed = match open {
        None => listed(rooms),
        Some(_) => None,
    };
    let (placeholder, empty) = match &open {
        Some(_) => (Text::key(StrId::ComposeHint), None),
        // Under the hub's rooms the field is the way to another (1652).
        None if listed.as_ref().is_some_and(|l| l.other.is_some()) => {
            (Text::key(StrId::RoomsJoinHint), None)
        }
        None => (
            Text::key(StrId::RoomsJoinHint),
            Some(Text::key(StrId::RoomsHowToJoin)),
        ),
    };
    let title = match (&open, rooms.hub) {
        (None, Some(_)) => on_hub(rooms),
        _ => Text::key(StrId::TabRooms),
    };
    RoomsNode {
        title,
        status,
        rows,
        open,
        input: rooms.input.clone(),
        placeholder,
        empty,
        retry,
        listed,
    }
}

/// "Connecting…", with its seconds once there were more than
/// [`LINKING_QUIET_SECS`] of them (1652).
/// How the link to the hub in force stands, in the rooms screen's words
/// (1652): "Not connected yet.", "Connecting… N s", "Connected", or why it
/// is down; `None` without a hub. The settings' row of the hub in force
/// says it too (1655, walk 9, 49).
pub fn link_words<B>(model: &UiModel<B>) -> Option<Text> {
    let rooms = &model.rooms;
    rooms.hub?;
    Some(match rooms.state {
        // "Never asked" is not "connecting" (1652).
        HubState::Off => Text::key(StrId::RoomsNotLinked),
        HubState::Linking => linking(rooms.linking_since, model.now),
        HubState::Ready => Text::key(StrId::RoomsLinked),
        HubState::Down(why) => down(why),
    })
}

fn linking(since: Option<i64>, now: Option<WallTime>) -> Text {
    let waited = match (since, now) {
        (Some(since), Some(now)) => now.unix_secs - since,
        _ => 0,
    };
    match waited {
        // The patience of the sheet (1659, walk 10, 53): the link may still
        // come, and its welcome still opens the screen.
        w if w >= ASK_PATIENCE_SECS => Text::key(StrId::RoomsNoAnswer),
        w if w >= SLOW_SECS => Text::Key(StrId::RoomsSlowFor, Vec::from([Arg::Int(w)])),
        w if w >= LINKING_QUIET_SECS => Text::Key(StrId::RoomsLinkingFor, Vec::from([Arg::Int(w)])),
        _ => Text::key(StrId::RoomsLinking),
    }
}

/// The link has been coming for [`ASK_PATIENCE_SECS`] (1659): the screen
/// says the server does not answer and offers to try again.
fn overdue(rooms: &Rooms, now: Option<WallTime>) -> bool {
    match (rooms.state, rooms.linking_since, now) {
        (HubState::Linking, Some(since), Some(now)) => now.unix_secs - since >= ASK_PATIENCE_SECS,
        _ => false,
    }
}

/// The rooms the chosen hub names that we are not in (1652): its `/list`
/// answer once it came; before it this run's sheet answer, merged with
/// what the mailbox's question found (1643) and the shipped list. Since
/// 1659 (walk 10, 52) they stand whatever the link does: a silent server's
/// screen said only "The server does not answer." under a bare "Rooms",
/// and a newcomer had to guess a name the store knew. A tap on one joins
/// it once the server answers ([`ask`] queues it and links). The title
/// names the server even when nothing is known of its rooms.
fn listed(rooms: &Rooms) -> Option<ListedNode> {
    let hub = rooms.hub?;
    // The link's own answer is the fresh one and stands alone; before it,
    // all that is known of the server.
    let known: Vec<String> = match &rooms.listed {
        Some(listed) => listed.clone(),
        None => {
            let named = match rooms.named.get(&hub) {
                Some(Named::Rooms(named)) => named.clone(),
                _ => Vec::new(),
            };
            let mailbox = rooms.mailbox_named.get(&hub).cloned().unwrap_or_default();
            let shipped = super::hubs::named(&hub)
                .map(|h| h.rooms)
                .unwrap_or_default();
            super::hubs::rooms_merged(&super::hubs::rooms_merged(&named, &mailbox), &shipped)
        }
    };
    let bare = |name: &str| name.trim_start_matches('#').to_lowercase();
    let names: Vec<String> = known
        .into_iter()
        .filter(|name| !rooms.rooms.iter().any(|r| bare(&r.name) == bare(name)))
        .collect();
    let note = match (names.is_empty(), &rooms.listed) {
        (false, _) => None,
        // Nothing known while the link is not up: the state says why.
        (true, _) if rooms.state != HubState::Ready => None,
        (true, None) => Some(Text::key(StrId::RoomsAsking)),
        (true, Some(all)) if all.is_empty() => Some(Text::key(StrId::RoomsNoneNamed)),
        // We are in every room it names: the joined rows say it.
        (true, Some(_)) => None,
    };
    Some(ListedNode {
        title: on_hub(rooms),
        other: (!names.is_empty()).then(|| Text::key(StrId::RoomsOther)),
        rooms: names,
        note,
    })
}

/// "Rooms on EuskalReticulum": the screen's title with no room open, and
/// the heading over the rooms the server names (1659, walk 10, 52).
fn on_hub(rooms: &Rooms) -> Text {
    Text::Key(
        StrId::RoomsOnHub,
        Vec::from([Arg::Text(alloc::boxed::Box::new(hub_label(rooms)))]),
    )
}

/// The open room as a chat.
fn view(room: &Room, people_shown: bool, offset_secs: i32) -> RoomView {
    let subtitle = match room.members.as_slice() {
        [] => Text::key(StrId::RoomsOnlyYou),
        names => Text::Key(StrId::RoomsWithYou, Vec::from([Arg::Str(names.join(", "))])),
    };
    let people = people_shown.then(|| {
        room.members
            .iter()
            .map(|m| Text::Raw(m.clone()))
            .chain(core::iter::once(Text::key(StrId::RoomsYou)))
            .collect()
    });
    let menu = Vec::from([
        RoomVerb {
            label: Text::key(StrId::RoomsPeople),
            event: RoomsEvent::People(!people_shown),
        },
        RoomVerb {
            label: Text::key(StrId::RoomsLeave),
            event: RoomsEvent::Leave,
        },
    ]);
    let mut lines: Vec<RoomLineNode> = Vec::with_capacity(room.lines.len());
    let mut last_writer: Option<(bool, Option<&str>)> = None;
    for line in &room.lines {
        let mut node = line_node(line, offset_secs);
        if node.bubble {
            let writer = (line.ours, line.nick.as_deref());
            node.named = !line.ours && last_writer != Some(writer);
            if node.named {
                node.avatar = line.nick.as_deref().map(avatar);
            }
            last_writer = Some(writer);
        } else {
            last_writer = None;
        }
        lines.push(node);
    }
    RoomView {
        name: room.name.clone(),
        avatar: avatar(&room.name),
        subtitle,
        people,
        people_note: Text::key(StrId::RoomsPeopleNote),
        menu,
        live: Text::key(StrId::RoomsLive),
        lines,
    }
}

fn line_node(line: &RoomLine, offset_secs: i32) -> RoomLineNode {
    let nick = |line: &RoomLine| match (&line.nick, line.ours) {
        (_, true) => Text::key(StrId::RoomsYou),
        (Some(nick), false) => Text::Raw(nick.clone()),
        (None, false) => Text::Raw(String::new()),
    };
    let (who, text) = match line.kind {
        // Our own join has its own sentence: the reader's "you" put into
        // a nick's sentence is no German (1384).
        RoomLineKind::Joined if line.ours => {
            (Text::Raw(String::new()), Text::key(StrId::RoomsWeCameIn))
        }
        RoomLineKind::Joined => (
            Text::Raw(String::new()),
            Text::Key(
                StrId::RoomsCameIn,
                Vec::from([Arg::Text(alloc::boxed::Box::new(nick(line)))]),
            ),
        ),
        RoomLineKind::Parted => (
            Text::Raw(String::new()),
            Text::Key(
                StrId::RoomsWentOut,
                Vec::from([Arg::Text(alloc::boxed::Box::new(nick(line)))]),
            ),
        ),
        RoomLineKind::Hub | RoomLineKind::Error => {
            (Text::Raw(String::new()), Text::Raw(line.text.clone()))
        }
        RoomLineKind::Said(_) => (nick(line), Text::Raw(line.text.clone())),
    };
    let bubble = matches!(line.kind, RoomLineKind::Said(_));
    let time = (bubble && line.ts_ms > 0).then(|| {
        WallTime {
            unix_secs: i64::try_from(line.ts_ms / 1000).unwrap_or(i64::MAX),
            offset_secs,
        }
        .hh_mm()
    });
    RoomLineNode {
        who,
        text,
        kind: line.kind,
        ours: line.ours,
        bubble,
        named: false,
        avatar: None,
        time,
    }
}

fn avatar_json(avatar: &Avatar) -> Value {
    json!({
        "initials": avatar.initials,
        "fg": alloc::format!("{:06x}", avatar.fg),
        "bg": alloc::format!("{:06x}", avatar.bg),
    })
}

impl RoomsNode {
    /// The surface as JSON for golden files, its texts unresolved.
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title.to_json(),
            "status": self.status.as_ref().map(Text::to_json),
            "rows": self.rows.iter().map(|r| json!({
                "name": r.name,
                "unread": r.unread,
                "selected": r.selected,
                "avatar": avatar_json(&r.avatar),
                "public": r.public.to_json(),
            })).collect::<Vec<_>>(),
            "open": self.open.as_ref().map(|room| json!({
                "name": room.name,
                "avatar": avatar_json(&room.avatar),
                "subtitle": room.subtitle.to_json(),
                "people": room.people.as_ref().map(|p| p.iter().map(Text::to_json).collect::<Vec<_>>()),
                "people_note": room.people_note.to_json(),
                "menu": room.menu.iter().map(|v| v.label.to_json()).collect::<Vec<_>>(),
                "live": room.live.to_json(),
                "lines": room.lines.iter().map(|l| json!({
                    "who": l.who.to_json(),
                    "text": l.text.to_json(),
                    "kind": alloc::format!("{:?}", l.kind),
                    "ours": l.ours,
                    "bubble": l.bubble,
                    "named": l.named,
                    "avatar": l.avatar.as_ref().map(avatar_json),
                    "time": l.time,
                })).collect::<Vec<_>>(),
            })),
            "input": self.input,
            "placeholder": self.placeholder.to_json(),
            "empty": self.empty.as_ref().map(Text::to_json),
            "retry": self.retry.as_ref().map(Text::to_json),
            "listed": self.listed.as_ref().map(|l| json!({
                "title": l.title.to_json(),
                "rooms": l.rooms,
                "note": l.note.as_ref().map(Text::to_json),
                "other": l.other.as_ref().map(Text::to_json),
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::scene::{LayoutClass, ScreenId};
    use crate::ui::tests::Field as Buffer;
    use crate::ui::{update, UiEvent};
    use alloc::vec;

    const HUB: [u8; 16] = [0x07; 16];

    fn rooms(model: &mut UiModel<Buffer>, event: RoomsEvent) -> Vec<UiEffect> {
        update(model, UiEvent::Rooms(event))
    }

    /// A model with the tab on, linked, welcomed, in `#lab`, in front.
    fn in_lab() -> UiModel<Buffer> {
        let mut model = UiModel::<Buffer>::new(&[]);
        model.rooms.enabled = true;
        model.rooms.hub = Some(HUB);
        update(&mut model, UiEvent::Tab(TabId::Rooms));
        // The first join links (1578), and tells once.
        rooms(&mut model, RoomsEvent::Typed("#lab".into()));
        rooms(&mut model, RoomsEvent::Submit);
        rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: Some("periculum".into()),
                body_limit: 350,
            },
        );
        rooms(
            &mut model,
            RoomsEvent::Joined {
                room: "#lab".into(),
                nick: None,
            },
        );
        model
    }

    /// 1577, since 1652: the settings choose a shipped hub: the tab
    /// appears, the config keeps it, and the choice links at once, telling
    /// the reader first that their name is visible, which the config keeps
    /// was said. Opening the tab while it links asks nothing more; a join
    /// meanwhile waits for the hub's welcome, which sends it.
    #[test]
    fn a_hub_chosen_shows_the_tab_and_the_first_link_says_the_name_is_seen() {
        let community = crate::ui::hubs::shipped()
            .into_iter()
            .find(|h| h.name == "RNS Community")
            .map(|h| h.hash)
            .expect("shipped");
        let mut model = UiModel::<Buffer>::new(&[]);
        // A reader with a name (without one the room shows none, 1629).
        model.profile.name = "Lew".into();
        assert!(!model.rooms.enabled);
        let effects = rooms(&mut model, RoomsEvent::Hub(HubChoice::Hub(community)));
        assert_eq!(
            effects,
            [
                UiEffect::RememberHash {
                    key: "rooms_hub",
                    hash: Some(community)
                },
                UiEffect::RememberHash {
                    key: "rooms_told",
                    hash: Some(community)
                },
                UiEffect::Rooms(RoomAsk::Open { hub: community })
            ],
            "kept, told, and linked at once (1652)"
        );
        assert!(model.rooms.enabled, "the tab appears at once");
        assert_eq!(model.rooms.state, HubState::Linking);
        assert_eq!(hub_label(&model.rooms), Text::Raw("RNS Community".into()));
        let told = model.current_toast().map(|t| t.text.clone());
        assert_eq!(
            told.as_deref(),
            Some("Your name is visible to everyone in the room."),
            "said with the link"
        );
        let effects = update(&mut model, UiEvent::Tab(TabId::Rooms));
        assert_eq!(effects, [], "linking already");
        assert!(say(&mut model, "#lab").is_empty(), "the join waits");
        assert!(model.rooms.input.is_empty(), "the join waits for the hub");
        let welcomed = rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: None,
                body_limit: 350,
            },
        );
        assert_eq!(
            welcomed,
            [UiEffect::Rooms(RoomAsk::Join {
                room: "#lab".into(),
                key: None
            })],
            "the join that waited"
        );
        rooms(
            &mut model,
            RoomsEvent::Joined {
                room: "#lab".into(),
                nick: None,
            },
        );
        rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: Some("rnscommunity".into()),
                body_limit: 350,
            },
        );
        rooms(
            &mut model,
            RoomsEvent::Joined {
                room: "general".into(),
                nick: None,
            },
        );
        // Back, and the link down: the next opening links, saying nothing.
        rooms(&mut model, RoomsEvent::Down(Down::Lost));
        update(&mut model, UiEvent::Tab(TabId::Chats));
        assert_eq!(
            update(&mut model, UiEvent::Tab(TabId::Rooms)),
            [UiEffect::Rooms(RoomAsk::Open { hub: community })],
            "told once"
        );
        rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: None,
                body_limit: 350,
            },
        );
        // Another hub: the link closes, the old rooms go, the new one
        // links, told once more: it is another hub that learns the name.
        let effects = rooms(&mut model, RoomsEvent::Hub(HubChoice::Hub(HUB)));
        assert_eq!(
            effects,
            [
                UiEffect::Rooms(RoomAsk::Close),
                UiEffect::RememberHash {
                    key: "rooms_hub",
                    hash: Some(HUB)
                },
                UiEffect::RememberHash {
                    key: "rooms_told",
                    hash: Some(HUB)
                },
                UiEffect::Rooms(RoomAsk::Open { hub: HUB })
            ]
        );
        assert!(model.rooms.rooms.is_empty() && !model.rooms.open);
        assert_eq!(model.rooms.state, HubState::Linking);
        assert_eq!(
            hub_label(&model.rooms),
            Text::key(StrId::RoomsHubOwn),
            "no hash"
        );
        // None: the link it was making closes, the tab goes, the key
        // comes out.
        let effects = rooms(&mut model, RoomsEvent::Hub(HubChoice::None));
        assert_eq!(
            effects,
            [
                UiEffect::Rooms(RoomAsk::Close),
                UiEffect::RememberHash {
                    key: "rooms_hub",
                    hash: None
                }
            ]
        );
        assert!(!model.rooms.enabled);
        assert_eq!(hub_label(&model.rooms), Text::key(StrId::RoomsHubNone));
    }

    /// 1577: an address pasted is read as one; what is none is said and
    /// changes nothing (the control); the same hub again asks nothing.
    #[test]
    fn a_pasted_hub_is_an_address_or_nothing_changes() {
        let mut model = UiModel::<Buffer>::new(&[]);
        assert!(rooms(
            &mut model,
            RoomsEvent::Hub(HubChoice::Address("lab".into()))
        )
        .is_empty());
        assert!(!model.rooms.enabled);
        assert_eq!(
            model.current_toast().map(|t| t.text.clone()).as_deref(),
            Some("That is no server's address.")
        );
        let effects = rooms(
            &mut model,
            RoomsEvent::Hub(HubChoice::Address(
                " <0707070707070707070707070707070707> ".into(),
            )),
        );
        assert!(effects.is_empty(), "33 digits are no address: {effects:?}");
        let effects = rooms(
            &mut model,
            RoomsEvent::Hub(HubChoice::Address(
                " 07070707070707070707070707070707 ".into(),
            )),
        );
        assert_eq!(
            effects,
            [
                UiEffect::RememberHash {
                    key: "rooms_hub",
                    hash: Some(HUB)
                },
                UiEffect::RememberHash {
                    key: "rooms_told",
                    hash: Some(HUB)
                },
                UiEffect::Rooms(RoomAsk::Open { hub: HUB })
            ]
        );
        assert!(rooms(&mut model, RoomsEvent::Hub(HubChoice::Hub(HUB))).is_empty());
    }

    /// 1577: a joined room says it is public, in the reader's language,
    /// with the globe beside the word where the frontend draws one.
    #[test]
    fn a_room_in_the_list_is_marked_public() {
        let model = in_lab();
        let node = node(&model);
        assert_eq!(node.rows[0].public, Text::key(StrId::RoomsPublic));
        assert_eq!(
            node.rows[0].public.resolve(crate::ui::strings::Lang::De),
            "öffentlich"
        );
        assert_eq!(PUBLIC_MARK, "\u{1F310}");
    }

    fn say(model: &mut UiModel<Buffer>, typed: &str) -> Vec<UiEffect> {
        rooms(model, RoomsEvent::Typed(typed.into()));
        rooms(model, RoomsEvent::Submit)
    }

    /// 1578, since 1652: opening the rooms of a chosen hub links, once;
    /// a join while it links waits, and a second waits with the first.
    /// Rooms joined before link again when they are opened, and a link
    /// that came back joins again what it was in.
    #[test]
    fn the_first_join_links_once_and_a_link_that_came_back_joins_again() {
        let mut model = UiModel::<Buffer>::new(&[]);
        // The control: no hub, no tab, nothing asked.
        assert!(update(&mut model, UiEvent::Tab(TabId::Rooms)).is_empty());
        assert!(!model.rooms.showing());
        model.rooms.enabled = true;
        model.rooms.hub = Some(HUB);
        // Told before (1577): the join links and says nothing more.
        model.rooms.told = Some(HUB);
        assert_eq!(
            update(&mut model, UiEvent::Tab(TabId::Rooms)),
            vec![UiEffect::Rooms(RoomAsk::Open { hub: HUB })],
            "opening links (1652)"
        );
        assert_eq!(model.rooms.state, HubState::Linking);
        assert!(say(&mut model, "#lab").is_empty(), "linking already");
        assert!(
            update(&mut model, UiEvent::Tab(TabId::Rooms)).is_empty(),
            "opened again while it links: once"
        );
        assert!(say(&mut model, "#garden").is_empty(), "linking already");
        assert_eq!(model.rooms.queued.len(), 2, "both joins wait");
        let welcomed = rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: None,
                body_limit: 350,
            },
        );
        assert_eq!(welcomed.len(), 2);
        assert!(model.rooms.queued.is_empty());

        let mut model = in_lab();
        rooms(&mut model, RoomsEvent::Down(Down::Lost));
        assert_eq!(model.rooms.state, HubState::Down(Down::Lost));
        assert!(model.current_toast().is_some(), "a broken link is said");
        update(&mut model, UiEvent::Tab(TabId::Chats));
        assert_eq!(
            update(&mut model, UiEvent::Tab(TabId::Rooms)),
            vec![UiEffect::Rooms(RoomAsk::Open { hub: HUB })]
        );
        let again = rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: None,
                body_limit: 350,
            },
        );
        assert_eq!(
            again,
            vec![UiEffect::Rooms(RoomAsk::Join {
                room: "#lab".into(),
                key: None
            })]
        );
    }

    #[test]
    fn the_input_line_joins_says_acts_and_parts() {
        let mut model = in_lab();
        assert_eq!(
            say(&mut model, "hello"),
            vec![UiEffect::Rooms(RoomAsk::Say {
                room: "#lab".into(),
                kind: LineKind::Msg,
                text: "hello".into()
            })]
        );
        assert!(model.rooms.input.is_empty(), "a sent line leaves the input");
        assert_eq!(
            say(&mut model, "/me waves"),
            vec![UiEffect::Rooms(RoomAsk::Say {
                room: "#lab".into(),
                kind: LineKind::Action,
                text: "waves".into()
            })]
        );
        assert_eq!(
            say(&mut model, "/join #keyed sesame"),
            vec![UiEffect::Rooms(RoomAsk::Join {
                room: "#keyed".into(),
                key: Some("sesame".into())
            })]
        );
        assert_eq!(
            say(&mut model, "/part"),
            vec![UiEffect::Rooms(RoomAsk::Part {
                room: "#lab".into()
            })]
        );
        rooms(
            &mut model,
            RoomsEvent::Parted {
                room: "#lab".into(),
                nick: None,
            },
        );
        assert!(model.rooms.rooms.is_empty() && model.rooms.selected.is_none());
        // With no room open, a room's name joins it; other words say how.
        assert_eq!(
            say(&mut model, "#lab"),
            vec![UiEffect::Rooms(RoomAsk::Join {
                room: "#lab".into(),
                key: None
            })]
        );
        assert!(say(&mut model, "hello").is_empty());
        assert!(model.current_toast().is_some());
    }

    #[test]
    fn a_line_past_the_limit_stays_and_is_said_before_anything_is_sent() {
        let mut model = in_lab();
        let long = "x".repeat(351);
        assert!(say(&mut model, &long).is_empty());
        assert_eq!(model.rooms.input, long, "kept to be shortened");
        let toast = model.current_toast().expect("said").text.clone();
        assert_eq!(
            toast,
            "This line is 351 bytes long and a room takes at most 350. Shorten it."
        );
        // The control: at the limit it goes.
        assert_eq!(say(&mut model, &"x".repeat(350)).len(), 1);
        // And before the hub answered no line goes: a join links (1578),
        // a room's line has no room to go to.
        let mut early = UiModel::<Buffer>::new(&[]);
        early.rooms.enabled = true;
        early.rooms.hub = Some(HUB);
        early.rooms.told = Some(HUB);
        assert_eq!(
            update(&mut early, UiEvent::Tab(TabId::Rooms)),
            vec![UiEffect::Rooms(RoomAsk::Open { hub: HUB })]
        );
        assert!(say(&mut early, "/part #lab").is_empty());
        assert!(say(&mut early, "#lab").is_empty(), "the join waits");
        assert_eq!(early.rooms.queued.len(), 1);
    }

    #[test]
    fn lines_while_away_are_unread_and_ours_never_are() {
        let mut model = in_lab();
        let line = |ours| RoomsEvent::Line {
            room: "#lab".into(),
            kind: LineKind::Msg,
            text: "hi".into(),
            nick: Some("bob".into()),
            ours,
            ts_ms: 1,
        };
        rooms(&mut model, line(false));
        assert_eq!(model.rooms.rooms[0].unread, 0, "the open room");
        update(&mut model, UiEvent::Tab(TabId::Chats));
        rooms(&mut model, line(false));
        rooms(&mut model, line(true));
        assert_eq!(model.rooms.rooms[0].unread, 1);
        let tabs = model.scene(LayoutClass::Wide).tabs.expect("tabs");
        let tab = tabs
            .items
            .iter()
            .find(|t| t.id == TabId::Explore)
            .expect("the tab: the rooms live under Explore (1578)");
        assert_eq!(tab.badge, Some(1));
        update(&mut model, UiEvent::Tab(TabId::Rooms));
        assert_eq!(model.rooms.rooms[0].unread, 0);
    }

    #[test]
    fn the_scene_is_the_rooms_with_the_one_line_when_a_room_is_open() {
        let mut model = in_lab();
        rooms(
            &mut model,
            RoomsEvent::Line {
                room: "#lab".into(),
                kind: LineKind::Msg,
                text: "hello bob".into(),
                nick: Some("lew".into()),
                ours: true,
                ts_ms: 1,
            },
        );
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Rooms);
        assert!(
            scene.list.is_none() && scene.chat.is_none(),
            "no conversation"
        );
        let node = scene.rooms.expect("the rooms");
        let open = node.open.expect("the open room");
        assert_eq!(open.live, Text::key(StrId::RoomsLive));
        let last = open.lines.last().expect("a line");
        assert_eq!(last.who, Text::key(StrId::RoomsYou));
        // 1450: a link that is up says nothing, and the field's hint is
        // a chat's.
        assert_eq!(node.status, None);
        assert_eq!(node.placeholder, Text::key(StrId::ComposeHint));
        assert_eq!(node.empty, None);
        // The control: with no room open the one line is not said.
        rooms(&mut model, RoomsEvent::Show(None));
        let node = model.scene(LayoutClass::Wide).rooms.expect("the rooms");
        assert!(node.open.is_none());
        assert_eq!(node.empty, Some(Text::key(StrId::RoomsHowToJoin)));
        assert_eq!(node.placeholder, Text::key(StrId::RoomsJoinHint));
        // And with the tab behind, the scene has no Rooms.
        update(&mut model, UiEvent::Tab(TabId::Chats));
        assert!(model.scene(LayoutClass::Wide).rooms.is_none());
    }

    /// 1384: our own join is a sentence of its own, since "du" put into
    /// a nick's "%1$s ist da" is no German; another's join is the nick's
    /// (the control).
    #[test]
    fn our_join_is_said_as_ours_and_anothers_with_the_nick() {
        use super::super::strings::Lang;
        let mut model = in_lab();
        rooms(
            &mut model,
            RoomsEvent::Joined {
                room: "#lab".into(),
                nick: Some("bob".into()),
            },
        );
        let node = node(&model);
        let lines = node.open.expect("the open room").lines;
        let said: Vec<String> = lines.iter().map(|l| l.text.resolve(Lang::De)).collect();
        assert_eq!(said, ["du bist da", "bob ist da"]);
        assert_eq!(lines[0].text.resolve(Lang::En), "you came in");
    }

    /// 1450 (walk 1, finding 20): a room is drawn as a chat. Said lines
    /// are bubbles, another's run is named once with its avatar, ours
    /// stand unnamed, the comings and goings are quiet lines between;
    /// the header names the people seen, and no word on screen is IRC's.
    #[test]
    fn a_room_is_drawn_as_a_chat_with_bubbles_avatars_and_its_people() {
        use super::super::strings::Lang;
        let mut model = in_lab();
        rooms(
            &mut model,
            RoomsEvent::Joined {
                room: "#lab".into(),
                nick: Some("bob".into()),
            },
        );
        for (nick, ours, text) in [
            ("bob", false, "morning"),
            ("bob", false, "anyone at the field site?"),
            ("lew", true, "on my way"),
            ("carol", false, "me too"),
        ] {
            rooms(
                &mut model,
                RoomsEvent::Line {
                    room: "#lab".into(),
                    kind: LineKind::Msg,
                    text: text.into(),
                    nick: Some(nick.into()),
                    ours,
                    ts_ms: 1_793_702_757_876,
                },
            );
        }
        let open = node(&model).open.expect("open");
        let shape: Vec<(bool, bool, bool)> = open
            .lines
            .iter()
            .map(|l| (l.bubble, l.named, l.avatar.is_some()))
            .collect();
        assert_eq!(
            shape,
            [
                (false, false, false), // you came in
                (false, false, false), // bob came in
                (true, true, true),    // bob, named
                (true, false, false),  // bob again, the same run
                (true, false, false),  // ours
                (true, true, true),    // carol
            ]
        );
        assert!(open.lines[2].time.is_some(), "a stamped line has its time");
        assert_eq!(open.subtitle.resolve(Lang::En), "bob, carol, you");
        assert!(open.people.is_none(), "behind the header until tapped");
        // The header's tap shows them, the reader last; Esc hides them.
        rooms(&mut model, RoomsEvent::People(true));
        let people: Vec<String> = node(&model)
            .open
            .and_then(|o| o.people)
            .expect("shown")
            .iter()
            .map(|t| t.resolve(Lang::En))
            .collect();
        assert_eq!(people, ["bob", "carol", "you"]);
        // Who left is gone from them.
        rooms(
            &mut model,
            RoomsEvent::Parted {
                room: "#lab".into(),
                nick: Some("bob".into()),
            },
        );
        let subtitle = node(&model).open.expect("open").subtitle;
        assert_eq!(subtitle.resolve(Lang::De), "carol, du");
        // No word of IRC's on the screen: the rooms' words in both
        // languages, the shortcuts and the hub named nowhere.
        for lang in [Lang::En, Lang::De] {
            let shown = node(&model);
            let mut words = vec![
                shown.placeholder.resolve(lang),
                shown.open.as_ref().expect("open").live.resolve(lang),
            ];
            words.extend(shown.status.iter().map(|t| t.resolve(lang)));
            words.extend(
                shown
                    .open
                    .iter()
                    .flat_map(|o| o.menu.iter().map(|v| v.label.resolve(lang))),
            );
            for word in words {
                for irc in ["/me", "/part", "/join", "Hub", "rrc"] {
                    assert!(!word.contains(irc), "{word:?} says {irc:?}");
                }
            }
        }
    }

    /// 1450: "Leave room" in the menu parts the open room as `/part`
    /// does, and before the hub answered it says so instead.
    #[test]
    fn leave_in_the_menu_parts_the_open_room() {
        let mut model = in_lab();
        let menu = node(&model).open.expect("open").menu;
        let leave = menu
            .iter()
            .find(|v| v.label == Text::key(StrId::RoomsLeave))
            .expect("Leave room in the menu");
        assert_eq!(
            rooms(&mut model, leave.event.clone()),
            vec![UiEffect::Rooms(RoomAsk::Part {
                room: "#lab".into()
            })]
        );
        // The control: no room open, nothing to leave.
        rooms(&mut model, RoomsEvent::Show(None));
        assert!(rooms(&mut model, RoomsEvent::Leave).is_empty());
        // The terminal's Alt-l is the same verb; Ctrl-l is not (it
        // redraws everywhere).
        rooms(&mut model, RoomsEvent::Show(Some(0)));
        use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert!(update(&mut model, UiEvent::Key(ctrl_l)).is_empty());
        let alt_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT);
        assert_eq!(
            update(&mut model, UiEvent::Key(alt_l)),
            vec![UiEffect::Rooms(RoomAsk::Part {
                room: "#lab".into()
            })]
        );
    }

    /// 1652: the words tell the state. Never asked is "not connected yet"
    /// with "Connect"; linking is "Connecting…", its seconds counted after
    /// five; a hub that did not answer says so in plain words and a tap on
    /// "Try again" links again.
    #[test]
    fn the_state_words_tell_the_truth_and_a_failure_can_be_retried() {
        use crate::ui::strings::Lang;
        let at = |secs| {
            Some(WallTime {
                unix_secs: secs,
                offset_secs: 0,
            })
        };
        let mut model = UiModel::<Buffer>::new(&[]);
        model.rooms.enabled = true;
        model.rooms.hub = Some(HUB);
        model.rooms.told = Some(HUB);
        model.now = at(1_000);
        let shown = node(&model);
        assert_eq!(
            shown.status.map(|t| t.resolve(Lang::De)).as_deref(),
            Some("Noch nicht verbunden.")
        );
        assert_eq!(shown.retry, Some(Text::key(StrId::RoomsConnect)));
        assert_eq!(
            rooms(&mut model, RoomsEvent::Retry),
            [UiEffect::Rooms(RoomAsk::Open { hub: HUB })]
        );
        let status = |model: &UiModel<Buffer>| node(model).status.map(|t| t.resolve(Lang::De));
        assert_eq!(status(&model).as_deref(), Some("Verbinde…"));
        model.now = at(1_004);
        assert_eq!(
            status(&model).as_deref(),
            Some("Verbinde…"),
            "quiet for five"
        );
        model.now = at(1_012);
        assert_eq!(status(&model).as_deref(), Some("Verbinde… 12 s"));
        assert_eq!(node(&model).retry, None);
        assert!(rooms(&mut model, RoomsEvent::Retry).is_empty(), "once");
        rooms(&mut model, RoomsEvent::Down(Down::NoAnswer));
        assert_eq!(status(&model).as_deref(), Some("Server antwortet nicht."));
        assert_eq!(node(&model).retry, Some(Text::key(StrId::RoomsRetry)));
        // Down, nothing known of its rooms: the title only, no note; the
        // state line says why (1659).
        let listed = node(&model).listed.expect("the title while it is down");
        assert!(listed.rooms.is_empty() && listed.note.is_none());
        model.now = at(2_000);
        assert_eq!(
            rooms(&mut model, RoomsEvent::Retry),
            [UiEffect::Rooms(RoomAsk::Open { hub: HUB })]
        );
        assert_eq!(status(&model).as_deref(), Some("Verbinde…"), "counted anew");
    }

    /// 1652: a hub that names no rooms says so, and the typed-name field
    /// stays the way in, with its sentence.
    #[test]
    fn a_hub_naming_no_rooms_says_so_and_keeps_the_field() {
        let mut model = UiModel::<Buffer>::new(&[]);
        model.rooms.enabled = true;
        model.rooms.hub = Some(HUB);
        update(&mut model, UiEvent::Tab(TabId::Rooms));
        rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: Some("ShadowLink".into()),
                body_limit: 350,
            },
        );
        rooms(&mut model, RoomsEvent::Listed { rooms: Vec::new() });
        let shown = node(&model);
        let listed = shown.listed.expect("the list's place");
        assert!(listed.rooms.is_empty());
        assert_eq!(
            listed
                .note
                .map(|t| t.resolve(crate::ui::strings::Lang::De))
                .as_deref(),
            Some("Dieser Server nennt keine öffentlichen Räume.")
        );
        assert_eq!(listed.other, None);
        assert_eq!(shown.empty, Some(Text::key(StrId::RoomsHowToJoin)));
        assert_eq!(
            model.rooms.named.get(&HUB),
            Some(&Named::Rooms(Vec::new())),
            "the sheet knows it too"
        );
        // A join by name still goes.
        assert_eq!(
            say(&mut model, "#lab"),
            [UiEffect::Rooms(RoomAsk::Join {
                room: "#lab".into(),
                key: None
            })]
        );
    }

    /// 1659, walk 10, 52: the chosen server's rooms screen while the
    /// server is silent (EuskalReticulum answered nothing all evening):
    /// the title names it, and the rooms known for it (the mailbox's
    /// question, 1643; this run's sheet, 1621) stand as rows a tap joins,
    /// with the state line above. The control: a server nothing is known
    /// of keeps the typed-name sentence.
    #[test]
    fn a_chosen_hub_with_stored_rooms_and_a_dead_link_lists_them() {
        use crate::ui::strings::Lang;
        let euskal = crate::address::parse("14d0b712d769a243e700209f6c6f5af4").expect("hash");
        let mut model = UiModel::<Buffer>::new(&[]);
        model.rooms.enabled = true;
        model.rooms.hub = Some(euskal);
        model.rooms.told = Some(euskal);
        model
            .rooms
            .mailbox_named
            .insert(euskal, vec!["general".to_string()]);
        update(&mut model, UiEvent::Tab(TabId::Rooms));
        rooms(&mut model, RoomsEvent::Down(Down::NoAnswer));
        let shown = node(&model);
        let listed = shown.listed.expect("the known rooms, the link down");
        assert_eq!(
            shown.title.resolve(Lang::De),
            "Räume auf EuskalReticulum",
            "the screen's title names the server"
        );
        assert_eq!(listed.title.resolve(Lang::De), "Räume auf EuskalReticulum");
        assert_eq!(listed.rooms, ["general"]);
        assert_eq!(
            shown.status.map(|t| t.resolve(Lang::De)).as_deref(),
            Some("Server antwortet nicht."),
            "the state above them"
        );
        assert_eq!(shown.empty, None, "no guessing a name");
        // A tap joins it once the server answers: it links again now.
        assert_eq!(
            rooms(&mut model, RoomsEvent::Join("general".into())),
            [UiEffect::Rooms(RoomAsk::Open { hub: euskal })]
        );
        assert_eq!(
            model.rooms.queued,
            [RoomAsk::Join {
                room: "general".into(),
                key: None
            }]
        );
        // This run's sheet answer counts too, merged with the mailbox's.
        model
            .rooms
            .named
            .insert(euskal, Named::Rooms(vec!["#lab".to_string()]));
        let listed = node(&model).listed.expect("the sheet's rooms");
        assert_eq!(listed.rooms, ["#lab", "general"]);
        // The control: nothing known, the field's sentence stays, the
        // title still names the server.
        model.rooms.named.clear();
        model.rooms.mailbox_named.clear();
        let shown = node(&model);
        let listed = shown.listed.expect("the title stays");
        assert!(listed.rooms.is_empty());
        assert_eq!(listed.title.resolve(Lang::De), "Räume auf EuskalReticulum");
        assert_eq!(shown.empty, Some(Text::key(StrId::RoomsHowToJoin)));
    }

    /// 1659, walk 10, 53: the rooms screen keeps the sheet's patience.
    /// "Connecting… N s" to 19 s, "The server is answering slowly… N s"
    /// from 20 s, at 45 s "The server does not answer." with "Try again"
    /// (the screen gave up at 30 s, Minuteman answered after 45 s); the
    /// tap links anew. A welcome that comes after 45 s still opens the
    /// screen. The state is said once: no toast while the screen shows it
    /// (55: it stood doubled), a toast while the screen is behind.
    #[test]
    fn the_rooms_screen_waits_forty_five_seconds_and_a_late_welcome_still_counts() {
        use crate::ui::strings::Lang;
        let at = |secs| {
            Some(WallTime {
                unix_secs: secs,
                offset_secs: 0,
            })
        };
        let mut model = UiModel::<Buffer>::new(&[]);
        model.rooms.enabled = true;
        model.rooms.hub = Some(HUB);
        model.rooms.told = Some(HUB);
        model.now = at(1_000);
        assert_eq!(
            update(&mut model, UiEvent::Tab(TabId::Rooms)),
            [UiEffect::Rooms(RoomAsk::Open { hub: HUB })]
        );
        let status = |model: &UiModel<Buffer>| node(model).status.map(|t| t.resolve(Lang::De));
        model.now = at(1_019);
        assert_eq!(status(&model).as_deref(), Some("Verbinde… 19 s"));
        model.now = at(1_020);
        assert_eq!(
            status(&model).as_deref(),
            Some("Server antwortet langsam… 20 s")
        );
        assert_eq!(node(&model).retry, None, "slow is no failure");
        model.now = at(1_044);
        assert_eq!(
            status(&model).as_deref(),
            Some("Server antwortet langsam… 44 s")
        );
        assert!(
            rooms(&mut model, RoomsEvent::Retry).is_empty(),
            "nothing to retry yet"
        );
        model.now = at(1_045);
        assert_eq!(status(&model).as_deref(), Some("Server antwortet nicht."));
        assert_eq!(node(&model).retry, Some(Text::key(StrId::RoomsRetry)));
        // The late welcome, before any retry: the screen is up.
        rooms(
            &mut model,
            RoomsEvent::Welcomed {
                name: None,
                body_limit: 350,
            },
        );
        assert_eq!(model.rooms.state, HubState::Ready);
        assert_eq!(status(&model), None);
        // Again, the retry this time: it links anew, counted from zero.
        rooms(&mut model, RoomsEvent::Down(Down::Lost));
        rooms(&mut model, RoomsEvent::Retry);
        model.now = at(1_100);
        assert_eq!(status(&model).as_deref(), Some("Server antwortet nicht."));
        assert_eq!(
            rooms(&mut model, RoomsEvent::Retry),
            [UiEffect::Rooms(RoomAsk::Open { hub: HUB })]
        );
        assert_eq!(status(&model).as_deref(), Some("Verbinde…"), "counted anew");
        // Said once: the screen in front has no toast for it ...
        model.toasts.clear();
        rooms(&mut model, RoomsEvent::Down(Down::NoAnswer));
        assert!(
            model.toasts.is_empty(),
            "the state line says it: {:?}",
            model.toasts
        );
        // ... the screen behind has.
        update(&mut model, UiEvent::Tab(TabId::Chats));
        rooms(&mut model, RoomsEvent::Down(Down::NoAnswer));
        assert_eq!(model.toasts.len(), 1);
    }
}
