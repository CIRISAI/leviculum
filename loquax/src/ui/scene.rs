//! The scene: what a screen shows and what each thing on it does, as one
//! value built from the model (1217, slice one of
//! `attachments/1217-loquax-one-ui-contract.md`; Lew's goal of 2026-10-31:
//! four targets from one code base, a decision made once appears on each).
//!
//! [`UiModel::scene`] builds it for a [`LayoutClass`]. A renderer (the
//! phone's Views, the window's iced widgets, the terminal's cells, the
//! M9's pixels) turns the nodes into its own drawing; an input adapter
//! turns a key, a click or a tap into an [`Intent`], a tap on a node into
//! the node's [`Action`]. The scene carries no geometry: no pixel, dp or
//! cell is in it, so each renderer keeps its spacing, animation and idiom.
//! The two classes differ in structure only where the contract allows it:
//! [`LayoutClass::Compact`] stacks the list and the chat, one at a time,
//! [`LayoutClass::Wide`] shows both. Inside a class every target shows the
//! same structure.
//!
//! The rule from slice one on: a feature lands here first, as a node or an
//! intent with its builder and a test; a target that cannot draw a node
//! shows its fallback (the node's text and its actions as a plain list)
//! and says so in the census.
//!
//! Slice one builds the list and the chat. The phone's facade derives its
//! `ConversationRow` and `MessageRow` from [`Row`] and [`BubbleNode`]
//! ([`row`], [`rows`], [`chat()`]), so what Java sees is byte for byte what
//! it saw before. [`Sheet`], [`Card`] and the scene's [`Chip`] exist as
//! types and stay empty until the map's chrome moves here (slice nine).
//!
//! Every node carries what a screen reader and `uiautomator` need
//! ([`A11y`]: a role, a label, a state, a stable resource id), so the
//! Android host can publish the nodes through an
//! `AccessibilityNodeProvider` (slice three builds the provider) and Java
//! needs no word of its own for them.
//!
//! **Open direction, not built** (Lew's rule of 2026-10-31: the core as
//! large as possible, the platform as small as necessary). A shared Rust
//! pixel renderer for the scene, as the map already draws into the phone's
//! Bitmap, could serve the window (softbuffer or an iced image), the M9
//! (its framebuffer) and Android (a Bitmap or a Surface), with Java
//! reduced to input. Against it stand text input (the IME; Java keeps
//! every text field as a real `EditText`, decided 2026-10-31), the
//! accessibility tree and the platform's scrolling physics. The decision
//! waits for the frame time and battery numbers of slices one and two.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{json, Map, Value};

use super::chat::{self, Bubble, ListRow, Placement};
use super::intent::{Intent, Messages, Switch, Target};
use super::liveness::{self, Liveness};
use super::strings::{Arg, StrId, Text};
use super::UiModel;
use crate::address::to_hex;
use crate::when::WallTime;

/// The two layout classes (decision 3): no third.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutClass {
    /// A phone, the M9, a terminal narrower than `layout::NARROW`: the
    /// list or the open chat, one at a time.
    Compact,
    /// A desktop window, a tablet, a wide terminal: the list beside the
    /// open chat.
    Wide,
}

/// A tab of the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TabId {
    Chats,
    Map,
}

/// A node's stable identity: a renderer diffs on it, hit tests to it, and
/// moves the keyboard's focus over it; [`NodeId::resource`] is its
/// accessibility resource id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeId {
    TopBar,
    Tabs,
    Tab(TabId),
    List,
    /// A conversation's row, by its peer.
    Row([u8; 16]),
    Chat,
    ChatBar,
    /// A group of bubbles, by its first bubble's key.
    Group(u64),
    /// A bubble, by its message's key on this device.
    Bubble(u64),
    Compose,
    Sheet,
    Card,
    Chip(u32),
}

impl NodeId {
    /// The id `uiautomator` and the device walk find the node by:
    /// lower case, digits and `_`, as an Android resource name is.
    pub fn resource(&self) -> String {
        match self {
            NodeId::TopBar => "top_bar".to_string(),
            NodeId::Tabs => "tabs".to_string(),
            NodeId::Tab(TabId::Chats) => "tab_chats".to_string(),
            NodeId::Tab(TabId::Map) => "tab_map".to_string(),
            NodeId::List => "list".to_string(),
            NodeId::Row(peer) => format!("row_{}", to_hex(peer)),
            NodeId::Chat => "chat".to_string(),
            NodeId::ChatBar => "chat_bar".to_string(),
            NodeId::Group(key) => format!("group_{key}"),
            NodeId::Bubble(key) => format!("bubble_{key}"),
            NodeId::Compose => "compose".to_string(),
            NodeId::Sheet => "sheet".to_string(),
            NodeId::Card => "card".to_string(),
            NodeId::Chip(n) => format!("chip_{n}"),
        }
    }
}

/// What a node is to a screen reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Bar,
    TabBar,
    Tab,
    List,
    Row,
    Chat,
    Group,
    Bubble,
    Button,
    TextField,
    Sheet,
    Card,
    Chip,
}

impl Role {
    pub fn word(self) -> &'static str {
        match self {
            Role::Bar => "bar",
            Role::TabBar => "tab_bar",
            Role::Tab => "tab",
            Role::List => "list",
            Role::Row => "row",
            Role::Chat => "chat",
            Role::Group => "group",
            Role::Bubble => "bubble",
            Role::Button => "button",
            Role::TextField => "text_field",
            Role::Sheet => "sheet",
            Role::Card => "card",
            Role::Chip => "chip",
        }
    }
}

/// What TalkBack and `uiautomator` need of a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct A11y {
    pub role: Role,
    pub label: Text,
    pub selected: bool,
    pub enabled: bool,
    /// The stable resource id ([`NodeId::resource`]).
    pub resource: String,
}

impl A11y {
    fn of(id: NodeId, role: Role, label: Text) -> Self {
        Self {
            role,
            label,
            selected: false,
            enabled: true,
            resource: id.resource(),
        }
    }
}

/// Something a node does: the intent a tap on it raises, whether it may
/// now, and its label.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub intent: Intent,
    pub enabled: bool,
    pub label_key: StrId,
}

impl Action {
    /// `intent`, enabled, with its own label.
    pub fn of(intent: Intent) -> Self {
        let label_key = intent.label();
        Self {
            intent,
            enabled: true,
            label_key,
        }
    }
}

/// The avatar's ring: the peer's position is live, or stopped lately
/// (1209, [`chat::ring`] gives its colour on a pixel screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ring {
    Accent,
    Secondary,
}

impl Ring {
    /// The ring's colour as the phone's `colors.xml` names it.
    pub fn word(self) -> &'static str {
        match self {
            Ring::Accent => "accent",
            Ring::Secondary => "secondary",
        }
    }

    pub fn of(state: liveness::State) -> Option<Self> {
        match state {
            liveness::State::Live => Some(Ring::Accent),
            liveness::State::Recent => Some(Ring::Secondary),
            liveness::State::Stale => None,
        }
    }

    /// The ring's colour on a pixel screen, of `colours` ([`chat::ring`]).
    pub fn colour(self, colours: &chat::Colours) -> u32 {
        match self {
            Ring::Accent => colours.accent,
            Ring::Secondary => colours.text_secondary,
        }
    }
}

/// A round avatar: letters on a ground, `0xrrggbb` each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Avatar {
    pub initials: String,
    pub fg: u32,
    pub bg: u32,
    pub ring: Option<Ring>,
}

impl Avatar {
    fn of(
        initials: String,
        peer: &[u8; 16],
        icon: Option<&(String, String)>,
        ring: Option<Ring>,
    ) -> Self {
        let (fg, bg) = chat::avatar_colours(peer, icon);
        Self {
            initials,
            fg,
            bg,
            ring,
        }
    }
}

/// What leads the top bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Lead {
    /// The reader's own avatar, which opens the profile, with the mailbox
    /// link's dot when the shell knows it (none in slice one: the link is
    /// the facade's, not the model's).
    Me { avatar: Avatar, link: Option<bool> },
    /// A sub view: back, and the peer's avatar.
    Back { back: Action, avatar: Avatar },
}

/// The bar at the top of a screen.
#[derive(Clone, Debug, PartialEq)]
pub struct TopBar {
    pub id: NodeId,
    pub lead: Lead,
    pub title: Text,
    pub actions: Vec<Action>,
    pub a11y: A11y,
}

/// A tab of the bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub label: Text,
    /// The count on it: chats with unread messages.
    pub badge: Option<u32>,
    pub action: Action,
    pub a11y: A11y,
}

/// The tab bar: none on a sub view ("two levels only").
#[derive(Clone, Debug, PartialEq)]
pub struct Tabs {
    pub id: NodeId,
    pub items: Vec<Tab>,
    pub active: TabId,
    pub a11y: A11y,
}

/// A conversation's row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: NodeId,
    /// The row's content as the model words it (name, last line, time,
    /// unread, liveness, pinned, muted, mark): the facade's
    /// `ConversationRow` is made of it.
    pub row: ListRow,
    pub avatar: Avatar,
    /// The last line in the catalogue's words when it is a report's
    /// (`live`, `3 h ago`); none for a text, whose line is [`ListRow::last`].
    pub live: Option<Text>,
    /// Whether its conversation is the open one.
    pub selected: bool,
    /// A tap opens the conversation.
    pub activate: Action,
    /// A long press or a right click.
    pub menu: Vec<Action>,
    pub a11y: A11y,
}

/// The conversation list.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub id: NodeId,
    pub rows: Vec<Row>,
    /// What shows when there is no row.
    pub empty: Text,
    pub a11y: A11y,
}

/// The line above a day's first bubble.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayLine {
    pub at: WallTime,
    pub words: Text,
}

/// A bubble with where it sits and what it offers.
#[derive(Clone, Debug, PartialEq)]
pub struct BubbleNode {
    pub id: NodeId,
    /// The message as the model words it: the facade's `MessageRow` is
    /// made of it.
    pub bubble: Bubble,
    /// Grouping, corners, the day line, whether the time and mark show
    /// ([`chat::placements`]): no renderer computes it again.
    pub place: Placement,
    /// The newest report's words (`live · every 10 min · 2 min ago`).
    pub live: Option<Text>,
    /// A long press or a right click.
    pub actions: Vec<Action>,
    pub a11y: A11y,
}

/// Bubbles of one sender, minutes apart, on one day ([`chat::joins`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: NodeId,
    pub ours: bool,
    /// Who wrote: the reader, or the peer's name.
    pub sender: Text,
    /// A new day begins with the group.
    pub day: Option<DayLine>,
    pub bubbles: Vec<BubbleNode>,
    pub a11y: A11y,
}

/// The compose bar: the text field is the platform's (an `EditText` on
/// the phone, decided 2026-10-31), the scene names its hint and its send.
#[derive(Clone, Debug, PartialEq)]
pub struct Compose {
    pub id: NodeId,
    pub hint: Text,
    pub send: Action,
    pub a11y: A11y,
}

/// The open conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Chat {
    pub id: NodeId,
    pub peer: [u8; 16],
    pub bar: TopBar,
    pub groups: Vec<Group>,
    pub compose: Compose,
    pub a11y: A11y,
}

/// A sheet over the screen: the friends, a long press's actions, a
/// question. Empty in slice one.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub id: NodeId,
    pub title: Text,
    pub rows: Vec<Row>,
    pub actions: Vec<Action>,
    pub a11y: A11y,
}

/// A card over the map: a pin's peer. Empty in slice one.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: NodeId,
    pub title: Text,
    pub body: Text,
    pub actions: Vec<Action>,
    pub a11y: A11y,
}

/// A transient state, never a status line. Empty in slice one.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub id: NodeId,
    pub text: Text,
    pub a11y: A11y,
}

/// Which screen the scene is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenId {
    Chats,
    Chat { peer: [u8; 16] },
}

/// One screen's nodes.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub class: LayoutClass,
    pub screen: ScreenId,
    pub top: TopBar,
    pub tabs: Option<Tabs>,
    /// The list: always at Wide, only without an open chat at Compact.
    pub list: Option<List>,
    pub chat: Option<Chat>,
    pub sheet: Option<Sheet>,
    pub card: Option<Card>,
    pub chips: Vec<Chip>,
}

impl<B> UiModel<B> {
    /// The scene of the list and the open chat for `class`.
    pub fn scene(&self, class: LayoutClass) -> Scene {
        let open = self.conversations.get(self.selected).map(|_| self.selected);
        let chat = open.map(|c| chat(self, c));
        let screen = match &chat {
            Some(chat) => ScreenId::Chat { peer: chat.peer },
            None => ScreenId::Chats,
        };
        Scene {
            class: LayoutClass::Wide,
            screen,
            top: me_bar(self),
            tabs: Some(tabs(self)),
            list: Some(list(self)),
            chat,
            sheet: None,
            card: None,
            chips: Vec::new(),
        }
        .in_class(class)
    }
}

impl Scene {
    /// The scene in `class`, from the Wide one (1218): Compact with a
    /// chat open is the chat alone, its bar on top, no tabs and no list;
    /// without one it is the Wide scene. A window that is resized moves
    /// between the classes with this and no model; a Compact scene with a
    /// chat open has lost its list and stays as it is.
    pub fn in_class(mut self, class: LayoutClass) -> Scene {
        if class == LayoutClass::Compact {
            if let Some(chat) = &self.chat {
                self.top = chat.bar.clone();
                self.tabs = None;
                self.list = None;
            }
        }
        self.class = class;
        self
    }

    /// What a tap or a click on node `id` does: a row's opening, a tab's
    /// switch, a bar's back, the compose bar's send. None for a node that
    /// has no tap of its own (a bubble's is its menu) or is not in the
    /// scene.
    pub fn action(&self, id: NodeId) -> Option<&Action> {
        fn back(bar: &TopBar) -> Option<&Action> {
            match &bar.lead {
                Lead::Back { back, .. } => Some(back),
                Lead::Me { .. } => None,
            }
        }
        match id {
            NodeId::Row(_) => self
                .list
                .as_ref()
                .and_then(|list| list.rows.iter().find(|row| row.id == id))
                .map(|row| &row.activate),
            NodeId::Tab(tab) => self
                .tabs
                .as_ref()
                .and_then(|tabs| tabs.items.iter().find(|t| t.id == tab))
                .map(|t| &t.action),
            NodeId::ChatBar => self.chat.as_ref().and_then(|chat| back(&chat.bar)),
            NodeId::TopBar => back(&self.top),
            NodeId::Compose => self.chat.as_ref().map(|chat| &chat.compose.send),
            _ => None,
        }
    }

    /// The bubble node `id` names in the open chat.
    pub fn bubble(&self, id: NodeId) -> Option<&BubbleNode> {
        self.chat
            .as_ref()?
            .groups
            .iter()
            .flat_map(|group| &group.bubbles)
            .find(|node| node.id == id)
    }
}

impl List {
    /// The rows the list's search `query` finds, in the list's order:
    /// every word of it in the name or the last line
    /// ([`chat::row_matches`]); the empty query finds all.
    pub fn found<'a>(&'a self, query: &'a str) -> impl Iterator<Item = &'a Row> + 'a {
        self.rows
            .iter()
            .filter(move |row| chat::row_matches(&row.row, query))
    }
}

/// The reader's bar over the list.
fn me_bar<B>(model: &UiModel<B>) -> TopBar {
    let avatar = match model.address {
        Some(own) => Avatar::of(
            chat::initials(model.names.shown(&own), &own),
            &own,
            None,
            None,
        ),
        None => Avatar {
            initials: String::new(),
            fg: 0xffffff,
            bg: chat::AVATAR_PALETTE[0],
            ring: None,
        },
    };
    TopBar {
        id: NodeId::TopBar,
        lead: Lead::Me { avatar, link: None },
        title: Text::key(StrId::TabChats),
        actions: Vec::from([
            Action::of(Intent::Search),
            Action::of(Intent::NewConversation),
        ]),
        a11y: A11y::of(NodeId::TopBar, Role::Bar, Text::key(StrId::TabChats)),
    }
}

/// The tab bar: Chats with the count of chats holding unread messages,
/// and the map.
fn tabs<B>(model: &UiModel<B>) -> Tabs {
    let unread = model
        .order()
        .into_iter()
        .filter(|&c| model.conversations[c].unread_rows() > 0)
        .count();
    let unread = u32::try_from(unread).unwrap_or(u32::MAX);
    let active = match model.map {
        Some(_) => TabId::Map,
        None => TabId::Chats,
    };
    let tab = |id: TabId, label: StrId, badge: Option<u32>, spoken: Text| {
        let mut a11y = A11y::of(NodeId::Tab(id), Role::Tab, spoken);
        a11y.selected = id == active;
        Tab {
            id,
            label: Text::key(label),
            badge,
            action: Action::of(Intent::Tab(id)),
            a11y,
        }
    };
    let chats_spoken = match unread {
        0 => Text::key(StrId::TabChats),
        n => Text::Key(StrId::TabChatsUnread, Vec::from([Arg::Int(i64::from(n))])),
    };
    Tabs {
        id: NodeId::Tabs,
        items: Vec::from([
            tab(
                TabId::Chats,
                StrId::TabChats,
                (unread > 0).then_some(unread),
                chats_spoken,
            ),
            tab(TabId::Map, StrId::Map, None, Text::key(StrId::Map)),
        ]),
        active,
        a11y: A11y::of(NodeId::Tabs, Role::TabBar, Text::key(StrId::TabChats)),
    }
}

/// The list as the model orders it.
fn list<B>(model: &UiModel<B>) -> List {
    List {
        id: NodeId::List,
        rows: rows(model, model.order()),
        empty: Text::key(StrId::NoConversations),
        a11y: A11y::of(NodeId::List, Role::List, Text::key(StrId::TabChats)),
    }
}

/// The rows for the conversations `order` names, in its order.
pub fn rows<B>(model: &UiModel<B>, order: Vec<usize>) -> Vec<Row> {
    order
        .into_iter()
        .map(|index| row(model, &model.conversations[index].peer))
        .collect()
}

/// The row for `peer`, empty when it has no conversation: a blocked peer
/// is listed whether it wrote or not.
pub fn row<B>(model: &UiModel<B>, peer: &[u8; 16]) -> Row {
    let row = chat::list_row(model, peer);
    let id = NodeId::Row(*peer);
    let now = model.now.map(|now| now.unix_secs);
    let live = (row.updates > 0).then(|| list_live(&row.liveness, now));
    let selected = model
        .conversations
        .get(model.selected)
        .is_some_and(|c| c.peer == *peer);
    let avatar = Avatar::of(
        row.initials.clone(),
        peer,
        row.icon.as_ref(),
        Ring::of(row.liveness.state),
    );
    let target = Target::Peer(*peer);
    let menu = Vec::from([
        Action::of(Intent::MarkUnread {
            peer: target,
            on: Switch::of(row.unread == 0),
        }),
        Action::of(Intent::Pin {
            peer: target,
            on: Switch::of(!row.pinned),
        }),
        Action::of(Intent::Mute {
            peer: target,
            secs: if row.muted { 0 } else { -1 },
        }),
        Action::of(Intent::Archive {
            peer: target,
            on: Switch::of(!model.show_archived),
        }),
        Action::of(Intent::Forget {
            peer: target,
            everywhere: false,
            confirmed: false,
        }),
    ]);
    let spoken = match row.last.is_empty() {
        true => row.name.clone(),
        false => format!("{}, {}", row.name, row.last),
    };
    let mut a11y = A11y::of(id, Role::Row, Text::Raw(spoken));
    a11y.selected = selected;
    Row {
        id,
        activate: Action::of(Intent::OpenChat { peer: *peer }),
        avatar,
        live,
        selected,
        menu,
        a11y,
        row,
    }
}

/// The open chat of conversation `c`: its bar, its bubbles grouped, its
/// compose. An index past the conversations gives an empty chat.
pub fn chat<B>(model: &UiModel<B>, c: usize) -> Chat {
    let peer = model.conversations.get(c).map_or([0; 16], |c| c.peer);
    let name = model.names.label(&peer);
    let messages = model
        .conversations
        .get(c)
        .map_or(&[][..], |c| c.messages.as_slice());
    let liveness = model.liveness(&peer);
    let avatar = Avatar::of(
        chat::initials(model.names.shown(&peer), &peer),
        &peer,
        chat::latest_appearance(messages).as_ref(),
        Ring::of(liveness.state),
    );
    let bar = TopBar {
        id: NodeId::ChatBar,
        lead: Lead::Back {
            back: Action::of(Intent::CloseChat),
            avatar,
        },
        title: Text::Raw(name.clone()),
        actions: Vec::from([Action::of(Intent::ShowMap { peer: Some(peer) })]),
        a11y: A11y::of(NodeId::ChatBar, Role::Bar, Text::Raw(name.clone())),
    };
    let bubbles = chat::bubbles(model, c);
    let places = chat::placements(&bubbles);
    let now = model.now;
    let mut groups: Vec<Group> = Vec::new();
    for (bubble, place) in bubbles.into_iter().zip(places) {
        let node = bubble_node(peer, bubble, place, now);
        match groups.last_mut() {
            Some(group) if place.joins_before => group.bubbles.push(node),
            _ => {
                let id = NodeId::Group(node.bubble.key);
                let ours = node.bubble.ours;
                let sender = match ours {
                    true => Text::key(StrId::QuoteYou),
                    false => Text::Raw(name.clone()),
                };
                let day = match (place.day, node.bubble.at) {
                    (true, Some(at)) => Some(DayLine {
                        at,
                        words: day_words(&at, now.as_ref()),
                    }),
                    _ => None,
                };
                groups.push(Group {
                    id,
                    ours,
                    a11y: A11y::of(id, Role::Group, sender.clone()),
                    sender,
                    day,
                    bubbles: Vec::from([node]),
                });
            }
        }
    }
    Chat {
        id: NodeId::Chat,
        peer,
        bar,
        groups,
        compose: Compose {
            id: NodeId::Compose,
            hint: Text::key(StrId::ComposeHint),
            send: Action::of(Intent::Send {
                to: Target::Peer(peer),
                body: None,
            }),
            a11y: A11y::of(
                NodeId::Compose,
                Role::TextField,
                Text::key(StrId::ComposeHint),
            ),
        },
        a11y: A11y::of(NodeId::Chat, Role::Chat, Text::Raw(name)),
    }
}

fn bubble_node(
    peer: [u8; 16],
    bubble: Bubble,
    place: Placement,
    now: Option<WallTime>,
) -> BubbleNode {
    let id = NodeId::Bubble(bubble.key);
    let now_secs = now.map(|now| now.unix_secs);
    let live = bubble.liveness.map(|l| row_live(&l, now_secs));
    let to = Target::Peer(peer);
    let mut actions = Vec::new();
    if let Some(target) = bubble.id {
        actions.push(Action::of(Intent::Reply {
            to,
            target,
            body: None,
        }));
        actions.push(Action::of(Intent::React {
            to,
            target: Some(target),
            content: None,
        }));
    }
    actions.push(Action::of(Intent::Copy));
    actions.push(Action::of(Intent::Delete {
        messages: Messages::Keys {
            peer,
            keys: Vec::from([bubble.key]),
        },
        confirmed: false,
    }));
    let spoken = match (&live, bubble.body.is_empty()) {
        (Some(live), true) => live.clone(),
        _ => Text::Raw(bubble.body.clone()),
    };
    BubbleNode {
        id,
        a11y: A11y::of(id, Role::Bubble, spoken),
        bubble,
        place,
        live,
        actions,
    }
}

/// An age in the catalogue's words: `now`, else `2 min ago`, `3 h ago`,
/// `4 d ago`, on the thresholds of [`liveness::ago`].
pub fn ago(secs: i64) -> Text {
    let amount = match secs {
        ..60 => return Text::key(StrId::LiveNow),
        60..3600 => Text::Key(StrId::MinutesAgo, Vec::from([Arg::Int(secs / 60)])),
        3600..172_800 => Text::Key(StrId::HoursAgo, Vec::from([Arg::Int(secs / 3600)])),
        _ => Text::Key(StrId::DaysAgo, Vec::from([Arg::Int(secs / 86_400)])),
    };
    Text::Key(StrId::LiveAgo, Vec::from([Arg::Text(Box::new(amount))]))
}

/// A cadence in the catalogue's words: `every 10 min`, `every 2 h`, on the
/// rounding of [`liveness::every`].
pub fn every(cadence_s: i64) -> Text {
    let amount = match cadence_s {
        ..3600 => Text::Key(
            StrId::MinutesAgo,
            Vec::from([Arg::Int((cadence_s + 30) / 60)]),
        ),
        _ => Text::Key(
            StrId::HoursAgo,
            Vec::from([Arg::Int((cadence_s + 1800) / 3600)]),
        ),
    };
    Text::Key(StrId::LiveEvery, Vec::from([Arg::Text(Box::new(amount))]))
}

/// [`Liveness::row_words`] in the catalogue's words: the newest report's
/// row.
pub fn row_live(l: &Liveness, now: Option<i64>) -> Text {
    let Some(at) = l.last_fix_at else {
        return Text::Raw(String::new());
    };
    let age = now.map(|now| ago(now.saturating_sub(at)));
    match (l.state, l.cadence_s, age) {
        (liveness::State::Live, Some(cadence), Some(age)) => {
            Text::Join(Vec::from([Text::key(StrId::Live), every(cadence), age]))
        }
        (liveness::State::Live, None, Some(age)) => {
            Text::Join(Vec::from([Text::key(StrId::Live), age]))
        }
        (_, _, Some(age)) => Text::Key(StrId::LastPosition, Vec::from([Arg::Text(Box::new(age))])),
        (_, _, None) => Text::Key(StrId::LastPosition, Vec::from([Arg::Str(String::new())])),
    }
}

/// [`Liveness::list_words`] in the catalogue's words: the list's line of
/// a conversation whose last row is a report.
pub fn list_live(l: &Liveness, now: Option<i64>) -> Text {
    match (l.state, l.last_fix_at, now) {
        (liveness::State::Live, _, _) => Text::key(StrId::Live),
        (_, Some(at), Some(now)) => ago(now.saturating_sub(at)),
        _ => Text::key(StrId::Position),
    }
}

/// The day line's words: today, yesterday, else the weekday and the date,
/// with the year when it is not this one ([`crate::when::date_label`]).
pub fn day_words(at: &WallTime, now: Option<&WallTime>) -> Text {
    let c = at.civil();
    if let Some(now) = now {
        match now.day() - at.day() {
            0 => return Text::key(StrId::Today),
            1 => return Text::key(StrId::Yesterday),
            _ => {}
        }
    }
    let weekday = Text::key(WEEKDAYS[c.weekday as usize % 7]);
    let month = Text::key(MONTHS[(c.month as usize).saturating_sub(1) % 12]);
    let mut args = Vec::from([
        Arg::Text(Box::new(weekday)),
        Arg::Int(i64::from(c.day)),
        Arg::Text(Box::new(month)),
    ]);
    match now.is_some_and(|now| now.civil().year == c.year) {
        true => Text::Key(StrId::Date, args),
        false => {
            args.push(Arg::Int(c.year));
            Text::Key(StrId::DateYear, args)
        }
    }
}

const WEEKDAYS: [StrId; 7] = [
    StrId::Monday,
    StrId::Tuesday,
    StrId::Wednesday,
    StrId::Thursday,
    StrId::Friday,
    StrId::Saturday,
    StrId::Sunday,
];

const MONTHS: [StrId; 12] = [
    StrId::January,
    StrId::February,
    StrId::March,
    StrId::April,
    StrId::May,
    StrId::June,
    StrId::July,
    StrId::August,
    StrId::September,
    StrId::October,
    StrId::November,
    StrId::December,
];

// The scene as JSON, for the golden files of `loquax/tests/golden`: keys
// sorted (serde_json's map is a BTreeMap without `preserve_order`), texts
// as their keys and arguments, not resolved, so one golden pins both
// languages' structure.

fn hex(bytes: &[u8]) -> Value {
    Value::String(to_hex(bytes))
}

fn opt<T>(value: Option<T>, f: impl FnOnce(T) -> Value) -> Value {
    value.map_or(Value::Null, f)
}

impl Text {
    pub fn to_json(&self) -> Value {
        match self {
            Text::Key(id, args) => json!({
                "key": id.key(),
                "args": args.iter().map(|arg| match arg {
                    Arg::Str(s) => Value::String(s.clone()),
                    Arg::Int(n) => json!(n),
                    Arg::Text(text) => text.to_json(),
                }).collect::<Vec<_>>(),
            }),
            Text::Raw(text) => json!({ "raw": text }),
            Text::Join(parts) => {
                json!({ "join": parts.iter().map(Text::to_json).collect::<Vec<_>>() })
            }
        }
    }
}

impl A11y {
    pub fn to_json(&self) -> Value {
        json!({
            "role": self.role.word(),
            "label": self.label.to_json(),
            "selected": self.selected,
            "enabled": self.enabled,
            "resource": self.resource,
        })
    }
}

impl Action {
    pub fn to_json(&self) -> Value {
        json!({
            "intent": format!("{:?}", self.intent),
            "enabled": self.enabled,
            "label_key": self.label_key.key(),
        })
    }
}

fn actions(list: &[Action]) -> Value {
    Value::Array(list.iter().map(Action::to_json).collect())
}

impl Avatar {
    pub fn to_json(&self) -> Value {
        json!({
            "initials": self.initials,
            "fg": format!("{:06x}", self.fg),
            "bg": format!("{:06x}", self.bg),
            "ring": opt(self.ring, |r| json!(format!("{r:?}"))),
        })
    }
}

impl TopBar {
    pub fn to_json(&self) -> Value {
        let lead = match &self.lead {
            Lead::Me { avatar, link } => {
                json!({ "me": { "avatar": avatar.to_json(), "link": link } })
            }
            Lead::Back { back, avatar } => {
                json!({ "back": { "action": back.to_json(), "avatar": avatar.to_json() } })
            }
        };
        json!({
            "id": self.id.resource(),
            "lead": lead,
            "title": self.title.to_json(),
            "actions": actions(&self.actions),
            "a11y": self.a11y.to_json(),
        })
    }
}

fn wall(at: Option<WallTime>) -> Value {
    opt(at, |at| json!(at.unix_secs))
}

fn liveness_json(l: &Liveness) -> Value {
    json!({
        "state": l.state.word(),
        "cadence_s": l.cadence_s,
        "last_fix_at": l.last_fix_at,
    })
}

impl Row {
    pub fn to_json(&self) -> Value {
        let r = &self.row;
        json!({
            "id": self.id.resource(),
            "name": r.name,
            "last": r.last,
            "ours": r.ours,
            "at": wall(r.at),
            "unread": r.unread,
            "updates": r.updates,
            "kind": r.kind,
            "reaction": r.reaction,
            "mark": opt(r.mark, |m| json!(m.word())),
            "pinned": r.pinned,
            "muted": r.muted,
            "liveness": liveness_json(&r.liveness),
            "avatar": self.avatar.to_json(),
            "live": opt(self.live.as_ref(), Text::to_json),
            "selected": self.selected,
            "activate": self.activate.to_json(),
            "menu": actions(&self.menu),
            "a11y": self.a11y.to_json(),
        })
    }
}

impl BubbleNode {
    pub fn to_json(&self) -> Value {
        let b = &self.bubble;
        json!({
            "id": self.id.resource(),
            "msg_id": opt(b.id, |id| hex(&id)),
            "ours": b.ours,
            "body": b.body,
            "mark": opt(b.mark, |m| json!(m.word())),
            "at": wall(b.at),
            "chips": b.chips.iter().map(|c| json!({
                "content": c.content, "count": c.count, "ours": c.ours,
            })).collect::<Vec<_>>(),
            "fix": opt(b.fix.as_ref(), |f| json!([f.location.latitude_e6, f.location.longitude_e6])),
            "quote": opt(b.quote.as_ref(), |q| json!({ "to": hex(&q.to), "ours": q.ours, "text": q.text })),
            "updates": b.updates,
            "since": wall(b.since),
            "liveness": opt(b.liveness.as_ref(), liveness_json),
            "place": {
                "joins_before": self.place.joins_before,
                "joins_after": self.place.joins_after,
                "day": self.place.day,
                "meta": self.place.meta,
            },
            "live": opt(self.live.as_ref(), Text::to_json),
            "actions": actions(&self.actions),
            "a11y": self.a11y.to_json(),
        })
    }
}

impl Chat {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "peer": hex(&self.peer),
            "bar": self.bar.to_json(),
            "groups": self.groups.iter().map(|g| json!({
                "id": g.id.resource(),
                "ours": g.ours,
                "sender": g.sender.to_json(),
                "day": opt(g.day.as_ref(), |d| json!({ "at": d.at.unix_secs, "words": d.words.to_json() })),
                "bubbles": g.bubbles.iter().map(BubbleNode::to_json).collect::<Vec<_>>(),
                "a11y": g.a11y.to_json(),
            })).collect::<Vec<_>>(),
            "compose": {
                "id": self.compose.id.resource(),
                "hint": self.compose.hint.to_json(),
                "send": self.compose.send.to_json(),
                "a11y": self.compose.a11y.to_json(),
            },
            "a11y": self.a11y.to_json(),
        })
    }
}

impl Scene {
    /// The scene as JSON with sorted keys, for golden files.
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("class".into(), json!(format!("{:?}", self.class)));
        let screen = match self.screen {
            ScreenId::Chats => json!("chats"),
            ScreenId::Chat { peer } => json!({ "chat": hex(&peer) }),
        };
        map.insert("screen".into(), screen);
        map.insert("top".into(), self.top.to_json());
        map.insert(
            "tabs".into(),
            opt(self.tabs.as_ref(), |t| {
                json!({
                    "id": t.id.resource(),
                    "active": format!("{:?}", t.active),
                    "items": t.items.iter().map(|tab| json!({
                        "id": format!("{:?}", tab.id),
                        "label": tab.label.to_json(),
                        "badge": tab.badge,
                        "action": tab.action.to_json(),
                        "a11y": tab.a11y.to_json(),
                    })).collect::<Vec<_>>(),
                    "a11y": t.a11y.to_json(),
                })
            }),
        );
        map.insert(
            "list".into(),
            opt(self.list.as_ref(), |l| {
                json!({
                    "id": l.id.resource(),
                    "rows": l.rows.iter().map(Row::to_json).collect::<Vec<_>>(),
                    "empty": l.empty.to_json(),
                    "a11y": l.a11y.to_json(),
                })
            }),
        );
        map.insert("chat".into(), opt(self.chat.as_ref(), Chat::to_json));
        map.insert(
            "sheet".into(),
            opt(self.sheet.as_ref(), |s| json!(s.id.resource())),
        );
        map.insert(
            "card".into(),
            opt(self.card.as_ref(), |c| json!(c.id.resource())),
        );
        map.insert(
            "chips".into(),
            Value::Array(self.chips.iter().map(|c| json!(c.id.resource())).collect()),
        );
        Value::Object(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::strings::Lang;
    use crate::ui::tests::Field as Line;
    use crate::ui::{Message, Signature, UiEvent};
    use leviculum_ui::delivery::DeliveryState;

    #[test]
    fn the_liveness_words_are_the_cores_in_either_language() {
        let live = Liveness {
            state: liveness::State::Live,
            cadence_s: Some(600),
            last_fix_at: Some(1_000),
        };
        let now = Some(1_000 + 125);
        assert_eq!(row_live(&live, now).resolve(Lang::En), live.row_words(now));
        assert_eq!(
            row_live(&live, now).resolve(Lang::De),
            "live · alle 10 Min. · vor 2 Min."
        );
        let stopped = Liveness {
            state: liveness::State::Recent,
            ..live
        };
        let later = Some(1_000 + 3 * 3600);
        assert_eq!(
            row_live(&stopped, later).resolve(Lang::En),
            "last position 3 h ago"
        );
        assert_eq!(
            list_live(&stopped, later).resolve(Lang::En),
            stopped.list_words(later)
        );
        assert_eq!(list_live(&live, now).resolve(Lang::De), "live");
        assert_eq!(ago(30).resolve(Lang::En), liveness::ago(30));
        assert_eq!(ago(4 * 86_400).resolve(Lang::En), liveness::ago(4 * 86_400));
    }

    #[test]
    fn the_day_line_says_today_yesterday_or_the_date() {
        let at = |unix_secs| WallTime {
            unix_secs,
            offset_secs: 3600,
        };
        // 2026-10-30 10:00 local, a Friday.
        let t = 1_793_350_800;
        let now = at(t);
        assert_eq!(day_words(&at(t), Some(&now)).resolve(Lang::De), "Heute");
        assert_eq!(
            day_words(&at(t - 86_400), Some(&now)).resolve(Lang::En),
            "Yesterday"
        );
        assert_eq!(
            day_words(&at(t - 6 * 86_400), Some(&now)).resolve(Lang::En),
            crate::when::date_label(&at(t - 6 * 86_400), &now)
        );
        assert_eq!(
            day_words(&at(t - 6 * 86_400), Some(&now)).resolve(Lang::De),
            "Samstag, 24. Oktober"
        );
        assert_eq!(
            day_words(&at(t - 365 * 86_400), Some(&now)).resolve(Lang::En),
            crate::when::date_label(&at(t - 365 * 86_400), &now)
        );
    }

    fn model_with(bodies: &[(bool, i64)]) -> UiModel<Line> {
        let peer = [0x42; 16];
        let mut model = UiModel::<Line>::new(&[]);
        super::super::update(
            &mut model,
            UiEvent::Clock(WallTime {
                unix_secs: 1_793_350_800 + 3600,
                offset_secs: 3600,
            }),
        );
        let index = model.conversation(peer);
        for (i, (outbound, at)) in bodies.iter().enumerate() {
            let body = format!("m{i}");
            model.conversations[index].messages.push(Message {
                outbound: *outbound,
                size: body.len(),
                body,
                id: None,
                delivery: outbound.then_some(DeliveryState::Delivered),
                signature: Signature::Verified,
                leg: None,
                unproven: false,
                at: Some(WallTime {
                    unix_secs: *at,
                    offset_secs: 3600,
                }),
                ledger: Vec::new(),
                last_leg: None,
                activity: i as u64 + 1,
                fields: Vec::new(),
            });
        }
        model
    }

    /// The positive control of the grouping: one bubble 181 s after the
    /// one before breaks the group, 180 s keeps it (`chat::GROUP_SECS`).
    #[test]
    fn a_group_breaks_past_three_minutes() {
        let t = 1_793_350_800;
        let joined = model_with(&[(false, t), (false, t + 180)]);
        let c = 0;
        assert_eq!(chat(&joined, c).groups.len(), 1);
        let broken = model_with(&[(false, t), (false, t + 181)]);
        let groups = chat(&broken, c).groups;
        assert_eq!(groups.len(), 2);
        assert!(groups[0].day.is_some() && groups[1].day.is_none());
    }

    #[test]
    fn compact_stacks_and_wide_splits() {
        let t = 1_793_350_800;
        let mut model = model_with(&[(false, t)]);
        model.selected = crate::ui::NONE_OPEN;
        let compact = model.scene(LayoutClass::Compact);
        assert_eq!(compact.screen, ScreenId::Chats);
        assert!(compact.list.is_some() && compact.tabs.is_some() && compact.chat.is_none());
        model.selected = 0;
        let compact = model.scene(LayoutClass::Compact);
        assert!(compact.list.is_none() && compact.tabs.is_none());
        assert_eq!(compact.top.id, NodeId::ChatBar);
        let wide = model.scene(LayoutClass::Wide);
        assert!(wide.list.is_some() && wide.tabs.is_some() && wide.chat.is_some());
        assert_eq!(wide.top.id, NodeId::TopBar);
        let rows = &wide
            .list
            .as_ref()
            .map(|l| l.rows.clone())
            .unwrap_or_default();
        assert!(rows[0].selected && rows[0].a11y.selected);
        assert_eq!(rows[0].a11y.resource, format!("row_{}", "42".repeat(16)));
    }

    /// A window resized moves its scene between the classes without the
    /// model (1218): the Wide scene put in Compact is the Compact scene
    /// the model builds, with a chat open and without; and what a click on
    /// a node does is the node's action: a row opens its chat, a tab
    /// switches, the stacked chat's bar goes back, the compose sends; a
    /// bubble has no tap of its own.
    #[test]
    fn the_wide_scene_in_compact_is_the_models_and_a_node_says_what_a_click_does() {
        let t = 1_793_350_800;
        let mut model = model_with(&[(false, t)]);
        let peer = [0x42; 16];
        for open in [0, crate::ui::NONE_OPEN] {
            model.selected = open;
            assert_eq!(
                model
                    .scene(LayoutClass::Wide)
                    .in_class(LayoutClass::Compact),
                model.scene(LayoutClass::Compact)
            );
        }
        model.selected = 0;
        let wide = model.scene(LayoutClass::Wide);
        let intent = |scene: &Scene, id| scene.action(id).map(|a| a.intent.clone());
        assert_eq!(
            intent(&wide, NodeId::Row(peer)),
            Some(Intent::OpenChat { peer })
        );
        assert_eq!(intent(&wide, NodeId::Row([7; 16])), None);
        assert_eq!(
            intent(&wide, NodeId::Tab(TabId::Map)),
            Some(Intent::Tab(TabId::Map))
        );
        assert_eq!(
            intent(&wide, NodeId::TopBar),
            None,
            "the list's bar has no back"
        );
        assert!(matches!(
            intent(&wide, NodeId::Compose),
            Some(Intent::Send { .. })
        ));
        let key = wide
            .chat
            .as_ref()
            .map(|c| c.groups[0].bubbles[0].bubble.key);
        let key = key.expect("one bubble");
        assert_eq!(intent(&wide, NodeId::Bubble(key)), None);
        assert!(wide.bubble(NodeId::Bubble(key)).is_some());
        let compact = wide.in_class(LayoutClass::Compact);
        assert_eq!(intent(&compact, NodeId::TopBar), Some(Intent::CloseChat));
        assert_eq!(intent(&compact, NodeId::Row(peer)), None, "no list");
    }

    /// The list's search is one rule for every target (1218): every word
    /// in the name or the last line, the list's order kept; the empty
    /// query finds all.
    #[test]
    fn the_lists_search_finds_every_word() {
        let t = 1_793_350_800;
        let model = model_with(&[(false, t)]);
        let list = list(&model);
        let name = list.rows[0].row.name.clone();
        let found = |q: &str| list.found(q).count();
        assert_eq!(found(""), 1);
        assert_eq!(found(&format!("m0 {}", name.to_uppercase())), 1);
        assert_eq!(found(&format!("{name} nowhere")), 0);
    }
}
