//! The Rooms (1379, #232): Reticulum Relay Chat beside the groups, not
//! among them. A room lives on a hub (`rrc.hub`), is a name, and is live:
//! no mailbox keeps it, a line nobody was there for is gone, and there
//! are no receipts and no ticks. So a room is no conversation: it has its
//! own tab, its own list and its own lines, and when one is opened the
//! surface says in one line what a room is not.
//!
//! The model knows the hub the config names, how the link to it stands,
//! the rooms the hub says we are in, and what was said in them while we
//! were there. One input line does it all, as IRC's does: in a room a
//! line goes to the room, `/me` makes it an action, `/part` leaves; a
//! room name (`#lab`, or `/join #lab key`) joins. What reaches the hub is
//! the shell's ([`UiEffect::Rooms`], `crate::rooms_link`), what comes back
//! is [`RoomsEvent`].

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_rrc::{LineKind, Refusal};
use leviculum_ui::input::InputBuffer;
use leviculum_ui::toast::ToastKind;
use serde_json::{json, Value};

use super::scene::TabId;
use super::strings::{Arg, StrId, Text};
use super::{UiEffect, UiModel};

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
}

impl Room {
    fn push(&mut self, line: RoomLine) {
        if self.lines.len() >= LINES_KEPT {
            self.lines.remove(0);
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
    /// The input line.
    pub input: String,
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
            input: String::new(),
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
}

/// A tab chosen: the Rooms come to the front, any other sends them back.
/// The first time, the shell links to the hub.
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
        TabId::Chats | TabId::Map | TabId::Browse => {
            model.rooms.open = false;
            Vec::new()
        }
    }
}

/// Link to the hub unless linked or linking.
fn link<B>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    let Some(hub) = model.rooms.hub else {
        return Vec::new();
    };
    match model.rooms.state {
        HubState::Linking | HubState::Ready => Vec::new(),
        HubState::Off | HubState::Down(_) => {
            model.rooms.state = HubState::Linking;
            Vec::from([UiEffect::Rooms(RoomAsk::Open { hub })])
        }
    }
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
    let rooms = &mut model.rooms;
    match event {
        RoomsEvent::Linking => rooms.state = HubState::Linking,
        RoomsEvent::Welcomed { name, body_limit } => {
            rooms.state = HubState::Ready;
            rooms.hub_name = name;
            rooms.body_limit = body_limit;
            // A link that came back joins again what it was in.
            let again: Vec<UiEffect> = rooms
                .rooms
                .iter()
                .map(|r| {
                    UiEffect::Rooms(RoomAsk::Join {
                        room: r.name.clone(),
                        key: None,
                    })
                })
                .collect();
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
            let text = down(why).resolve(model.lang);
            if why != Down::Left {
                model.toast(ToastKind::Error, text);
            }
        }
        RoomsEvent::Show(index) => {
            rooms.selected = index.filter(|i| *i < rooms.rooms.len());
            if let Some(room) = rooms.selected.and_then(|i| rooms.rooms.get_mut(i)) {
                room.unread = 0;
            }
        }
        RoomsEvent::Typed(text) => rooms.input = text,
        RoomsEvent::Submit => return submit(model),
    }
    Vec::new()
}

/// A key while the Rooms tab is in front (the terminal; a phone and a
/// window have their own field and say [`RoomsEvent::Typed`]): letters
/// go to the input line, `Backspace` takes one back, `Enter` sends it,
/// `Up` and `Down` step the rooms, `Esc` closes the open room and, with
/// none open, leaves the tab for the chats, as `Ctrl-g` does.
pub(super) fn key<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    key: leviculum_ui::input::KeyEvent,
) -> Vec<UiEffect> {
    use leviculum_ui::input::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let rooms = &mut model.rooms;
    match key.code {
        KeyCode::Char('g') if ctrl => return leave(model),
        KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
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
        return Vec::new();
    }
    let open = model.rooms.room().map(|r| r.name.clone());
    let ask = if let Some(rest) = input.strip_prefix("/join ") {
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
    let Some(ask) = ask else {
        let text = Text::key(StrId::RoomsHowToJoin).resolve(model.lang);
        model.toast(ToastKind::Info, text);
        return Vec::new();
    };
    if model.rooms.state != HubState::Ready {
        let text = refused(&Refusal::NotReady).resolve(model.lang);
        model.toast(ToastKind::Error, text);
        return Vec::new();
    }
    model.rooms.input.clear();
    Vec::from([UiEffect::Rooms(ask)])
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

/// The Rooms surface as the frontends draw it: the hub's line, the rooms,
/// the open room's lines under the one line that says what a room is not,
/// and the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomsNode {
    /// How the hub stands.
    pub hub: Text,
    pub rows: Vec<RoomRow>,
    pub open: Option<RoomView>,
    pub input: String,
    /// What the input does here.
    pub hint: Text,
}

/// A room in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRow {
    pub name: String,
    pub unread: u32,
    pub selected: bool,
}

/// The open room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomView {
    pub name: String,
    /// "A room is live: …", the one line said when a room is opened.
    pub live: Text,
    pub lines: Vec<RoomLineNode>,
}

/// One drawn line of a room: who, and what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomLineNode {
    /// The nick, or the reader's own word ("you"), empty for the hub.
    pub who: Text,
    pub text: Text,
    pub kind: RoomLineKind,
    pub ours: bool,
}

/// The Rooms surface for the frontends.
pub fn node<B>(model: &UiModel<B>) -> RoomsNode {
    let rooms = &model.rooms;
    let hub = match rooms.state {
        _ if rooms.hub.is_none() => Text::key(StrId::RoomsNoHub),
        HubState::Off | HubState::Linking => Text::key(StrId::RoomsLinking),
        HubState::Ready => Text::Key(
            StrId::RoomsHub,
            Vec::from([Arg::Str(
                rooms.hub_name.clone().unwrap_or_else(|| "?".to_string()),
            )]),
        ),
        HubState::Down(why) => down(why),
    };
    let rows = rooms
        .rooms
        .iter()
        .enumerate()
        .map(|(i, r)| RoomRow {
            name: r.name.clone(),
            unread: r.unread,
            selected: rooms.selected == Some(i),
        })
        .collect();
    let open = rooms.room().map(|room| RoomView {
        name: room.name.clone(),
        live: Text::key(StrId::RoomsLive),
        lines: room.lines.iter().map(line_node).collect(),
    });
    let hint = Text::key(match rooms.room() {
        Some(_) => StrId::RoomsHintRoom,
        None => StrId::RoomsHowToJoin,
    });
    RoomsNode {
        hub,
        rows,
        open,
        input: rooms.input.clone(),
        hint,
    }
}

fn line_node(line: &RoomLine) -> RoomLineNode {
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
    RoomLineNode {
        who,
        text,
        kind: line.kind,
        ours: line.ours,
    }
}

impl RoomsNode {
    /// The surface as JSON for golden files, its texts unresolved.
    pub fn to_json(&self) -> Value {
        json!({
            "hub": self.hub.to_json(),
            "rows": self.rows.iter().map(|r| json!({
                "name": r.name,
                "unread": r.unread,
                "selected": r.selected,
            })).collect::<Vec<_>>(),
            "open": self.open.as_ref().map(|room| json!({
                "name": room.name,
                "live": room.live.to_json(),
                "lines": room.lines.iter().map(|l| json!({
                    "who": l.who.to_json(),
                    "text": l.text.to_json(),
                    "kind": alloc::format!("{:?}", l.kind),
                    "ours": l.ours,
                })).collect::<Vec<_>>(),
            })),
            "input": self.input,
            "hint": self.hint.to_json(),
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

    fn say(model: &mut UiModel<Buffer>, typed: &str) -> Vec<UiEffect> {
        rooms(model, RoomsEvent::Typed(typed.into()));
        rooms(model, RoomsEvent::Submit)
    }

    #[test]
    fn the_tab_links_once_and_a_link_that_came_back_joins_again() {
        let mut model = UiModel::<Buffer>::new(&[]);
        // The control: no hub, no tab, nothing asked.
        assert!(update(&mut model, UiEvent::Tab(TabId::Rooms)).is_empty());
        assert!(!model.rooms.showing());
        model.rooms.enabled = true;
        model.rooms.hub = Some(HUB);
        assert_eq!(
            update(&mut model, UiEvent::Tab(TabId::Rooms)),
            vec![UiEffect::Rooms(RoomAsk::Open { hub: HUB })]
        );
        assert_eq!(model.rooms.state, HubState::Linking);
        update(&mut model, UiEvent::Tab(TabId::Chats));
        assert!(
            update(&mut model, UiEvent::Tab(TabId::Rooms)).is_empty(),
            "linking already"
        );

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
        // And before the hub answered nothing goes.
        let mut early = UiModel::<Buffer>::new(&[]);
        early.rooms.enabled = true;
        early.rooms.hub = Some(HUB);
        update(&mut early, UiEvent::Tab(TabId::Rooms));
        assert!(say(&mut early, "#lab").is_empty());
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
            .find(|t| t.id == TabId::Rooms)
            .expect("the tab");
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
        assert_eq!(
            node.hub.resolve(super::super::strings::Lang::De),
            "Hub periculum"
        );
        // The control: with no room open the one line is not said.
        rooms(&mut model, RoomsEvent::Show(None));
        let node = model.scene(LayoutClass::Wide).rooms.expect("the rooms");
        assert!(node.open.is_none());
        assert_eq!(node.hint, Text::key(StrId::RoomsHowToJoin));
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
}
