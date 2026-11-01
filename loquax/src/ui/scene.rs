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
pub use leviculum_ui::scene::Role;
use leviculum_ui::scene::{actions_json as actions, Contract, Node, ToJson};

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
    /// The map pin in a row, by its peer (1224).
    Pin([u8; 16]),
    Chat,
    ChatBar,
    /// A group of bubbles, by its first bubble's key.
    Group(u64),
    /// A bubble, by its message's key on this device.
    Bubble(u64),
    /// The delivery mark of one of ours, by its message's key (1232).
    Mark(u64),
    /// A bubble's photo, by its message's key (1220).
    Photo(u64),
    /// A bubble's file, by its message's key and its place among the
    /// message's files (1221).
    File(u64, usize),
    /// A bubble's voice message, by its message's key (1222).
    Voice(u64),
    Compose,
    /// The compose bar's microphone (1222).
    Record,
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
            NodeId::Pin(peer) => format!("pin_{}", to_hex(peer)),
            NodeId::Chat => "chat".to_string(),
            NodeId::ChatBar => "chat_bar".to_string(),
            NodeId::Group(key) => format!("group_{key}"),
            NodeId::Bubble(key) => format!("bubble_{key}"),
            NodeId::Mark(key) => format!("mark_{key}"),
            NodeId::Photo(key) => format!("photo_{key}"),
            NodeId::File(key, index) => format!("file_{key}_{index}"),
            NodeId::Voice(key) => format!("voice_{key}"),
            NodeId::Compose => "compose".to_string(),
            NodeId::Record => "record".to_string(),
            NodeId::Sheet => "sheet".to_string(),
            NodeId::Card => "card".to_string(),
            NodeId::Chip(n) => format!("chip_{n}"),
        }
    }
}

impl Node for NodeId {
    fn resource(&self) -> String {
        NodeId::resource(self)
    }
}

/// Loquax's names in the shared scene vocabulary
/// (`leviculum_ui::scene`, 1240): its catalogue, its intents, its nodes,
/// its tabs, its avatar, and its conversation's row in a sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loquax;

impl Contract for Loquax {
    type Key = StrId;
    type Intent = Intent;
    type Node = NodeId;
    type Tab = TabId;
    type Avatar = Avatar;
    type Row = Row;
}

/// What TalkBack and `uiautomator` need of a node.
pub type A11y = leviculum_ui::scene::A11y<StrId>;

/// Something a node does: the intent a tap on it raises, whether it may
/// now, and its label.
pub type Action = leviculum_ui::scene::Action<Intent>;

/// What leads the top bar: the reader's avatar ([`Lead::Me`], with the
/// mailbox link's dot when the shell knows it; none in slice one, the link
/// is the facade's, not the model's), or back and the peer's avatar.
pub type Lead = leviculum_ui::scene::Lead<Loquax>;

/// The bar at the top of a screen.
pub type TopBar = leviculum_ui::scene::TopBar<Loquax>;

/// A tab of the bar.
pub type Tab = leviculum_ui::scene::Tab<Loquax>;

/// The tab bar: none on a sub view ("two levels only").
pub type Tabs = leviculum_ui::scene::Tabs<Loquax>;

/// A row's generic part: its id, whether its conversation is the open
/// one, its tap and its menu, its accessibility.
pub type ListItem = leviculum_ui::scene::ListItem<Loquax>;

/// A sheet over the screen: the friends, a long press's actions, a
/// question. Empty in slice one.
pub type Sheet = leviculum_ui::scene::Sheet<Loquax>;

/// A transient state, never a status line. Empty in slice one.
pub type Chip = leviculum_ui::scene::Chip<Loquax>;

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

/// A row's map pin (1224): the peer's newest position is fresh, younger
/// than [`liveness::RECENT_S`] (the Friends sheet's day of 1200). It
/// stands where the avatar's ring stood in the list (one element, one
/// meaning): tinted as the ring was, live in the accent, recent in the
/// secondary text colour. A tap opens the map on the peer, the rest of
/// the row still opens the chat.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionBadge {
    pub id: NodeId,
    /// How old the newest fix is, seconds; none without a clock.
    pub age_s: Option<i64>,
    pub liveness: liveness::State,
    pub tint: Ring,
    /// [`Intent::OpenMapAt`] on the peer.
    pub open: Action,
    /// `Position of Anna, 2 min ago`.
    pub a11y: A11y,
}

impl PositionBadge {
    /// The pin of `peer`, named `name`, whose position is `l` at `now`;
    /// none for a stale or an absent one. Without a clock the stream's
    /// state decides, which is at most recent then.
    pub fn of(peer: &[u8; 16], name: &str, l: &Liveness, now: Option<i64>) -> Option<Self> {
        let at = l.last_fix_at?;
        let age_s = now.map(|now| now.saturating_sub(at).max(0));
        let fresh = match age_s {
            Some(age) => age < liveness::RECENT_S,
            None => l.state != liveness::State::Stale,
        };
        if !fresh {
            return None;
        }
        // Live is a stream's word, not an age's: a stream of reports a day
        // apart is live for two, and its pin goes after one all the same.
        let tint = match l.state {
            liveness::State::Live => Ring::Accent,
            _ => Ring::Secondary,
        };
        let id = NodeId::Pin(*peer);
        let words = match (age_s, l.state) {
            (Some(age), _) => ago(age),
            (None, liveness::State::Live) => Text::key(StrId::Live),
            (None, _) => Text::key(StrId::Position),
        };
        let label = Text::Key(
            StrId::PositionOf,
            Vec::from([Arg::Str(name.to_string()), Arg::Text(Box::new(words))]),
        );
        Some(Self {
            id,
            age_s,
            liveness: l.state,
            tint,
            open: Action::of(Intent::OpenMapAt {
                peer: Target::Peer(*peer),
            }),
            a11y: A11y::of(id, Role::Button, label),
        })
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

/// A conversation's row: the shared [`ListItem`] (id, selected, tap,
/// menu, accessibility) and the conversation's own content beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub item: ListItem,
    /// The row's content as the model words it (name, last line, time,
    /// unread, liveness, pinned, muted, mark): the facade's
    /// `ConversationRow` is made of it.
    pub row: ListRow,
    pub avatar: Avatar,
    /// The last line in the catalogue's words when it is a report's
    /// (`live`, `3 h ago`); none for a text, whose line is [`ListRow::last`].
    pub live: Option<Text>,
    /// The map pin, for a peer whose position is fresh (1224); the row's
    /// avatar has no ring then or ever.
    pub position: Option<PositionBadge>,
    /// `Draft: …` (1219), which stands in for the last line, in italics,
    /// while a message typed in a conversation not open is not sent.
    pub draft: Option<Text>,
}

/// A row is its list item: `row.selected`, `row.activate`, `row.a11y`
/// read the shared part as they read the row's own fields before 1240.
impl core::ops::Deref for Row {
    type Target = ListItem;

    fn deref(&self) -> &ListItem {
        &self.item
    }
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
    /// Ours only: the ladder's mark, what a screen reader says for it,
    /// and for a failed one the tap that sends it again (1232).
    pub mark: Option<MarkNode>,
    /// The photo it carries (1220): the renderer draws its thumbnail
    /// (`crate::photo::thumbnail`), a tap shows it whole.
    pub photo: Option<PhotoNode>,
    /// The files it carries (1221): a row each, its name, size and the
    /// tap that saves and opens it.
    pub files: Vec<FileNode>,
    /// The voice message it carries (1222): its play or pause button,
    /// length and level bar.
    pub voice: Option<VoiceNode>,
    /// A long press or a right click.
    pub actions: Vec<Action>,
    pub a11y: A11y,
}

/// A sent message's mark on the ladder of loquax-read-receipts.md,
/// section 3.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkNode {
    pub mark: chat::Mark,
    /// `Sending`, `Read at 10:32`, `Not sent. Double tap to send again`.
    pub words: Text,
    /// [`Intent::SendAgain`] on [`chat::Mark::Failed`], else none.
    pub activate: Option<Action>,
}

/// A bubble's photo: what it is, what a screen reader says, and the tap.
#[derive(Clone, Debug, PartialEq)]
pub struct PhotoNode {
    pub id: NodeId,
    pub format: String,
    pub bytes: usize,
    /// `Photo, 312 KB`.
    pub words: Text,
    /// [`Intent::ViewPhoto`].
    pub open: Action,
}

/// A bubble's file: its name, size, what a screen reader says, and the
/// tap (1221).
#[derive(Clone, Debug, PartialEq)]
pub struct FileNode {
    pub id: NodeId,
    /// The name the sender wrote, as [`super::files::safe_name`] makes it
    /// safe to show and save: a renderer never sees a path or a control
    /// character of a stranger's.
    pub name: String,
    pub bytes: usize,
    /// `312 KB`: the row's second line.
    pub size: String,
    /// `File report.pdf, 312 KB`.
    pub words: Text,
    /// [`Intent::OpenFile`].
    pub open: Action,
}

/// A bubble's voice message (1222): what the player shows and the tap.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceNode {
    pub id: NodeId,
    /// The sender's audio mode (`crate::ui::voice::AM_OPUS_OGG`, ...).
    pub mode: u8,
    pub bytes: usize,
    pub duration_ms: u32,
    /// `0:07`.
    pub duration: String,
    /// The level bar: [`super::voice::BARS`] heights from 0 to 100, empty
    /// where the stream does not tell; a renderer draws an even bar then.
    pub levels: Vec<u8>,
    /// Whether it plays right now: the button shows pause.
    pub playing: bool,
    /// [`Intent::PlayVoice`], or [`Intent::PauseVoice`] while it plays;
    /// none for a mode this build does not play.
    pub toggle: Option<Action>,
    /// `Voice message, 0:07`.
    pub words: Text,
}

/// The screen reader's words for `mark`; `read_at` is the reader's time
/// for [`chat::Mark::Read`], shown in the zone `now` is in.
pub fn mark_words(mark: chat::Mark, read_at: Option<u32>, now: Option<WallTime>) -> Text {
    match mark {
        chat::Mark::Sending => Text::key(StrId::StateSending),
        chat::Mark::Unconfirmed => Text::key(StrId::StateUnconfirmed),
        chat::Mark::Mailbox => Text::key(StrId::StateMailbox),
        chat::Mark::Delivered => Text::key(StrId::StateDelivered),
        chat::Mark::Read => {
            let at = WallTime {
                unix_secs: i64::from(read_at.unwrap_or(0)),
                offset_secs: now.map_or(0, |now| now.offset_secs),
            };
            Text::Key(StrId::StateRead, Vec::from([Arg::Str(at.hh_mm())]))
        }
        chat::Mark::Failed => Text::key(StrId::StateFailed),
    }
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
/// the phone, decided 2026-10-31), the scene names its hint and its send,
/// and what the field holds when the chat opens: the conversation's draft
/// (1219), which the field reports back with [`Intent::Draft`] on every
/// change.
#[derive(Clone, Debug, PartialEq)]
pub struct Compose {
    pub id: NodeId,
    pub hint: Text,
    /// The draft's text.
    pub text: String,
    /// The message a reply being written answers.
    pub reply: Option<[u8; 32]>,
    pub send: Action,
    /// The microphone (1222): held, it records; released, the platform
    /// fills [`Intent::SendVoice`] with what it recorded; slid away, it
    /// cancels. Its words are the hint to hold it.
    pub record: Action,
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

/// A card over the map: a pin's peer. Empty in slice one.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: NodeId,
    pub title: Text,
    pub body: Text,
    pub actions: Vec<Action>,
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
                .and_then(|list| list.rows.iter().find(|row| row.item.id == id))
                .map(|row| &row.item.activate),
            NodeId::Pin(peer) => self
                .list
                .as_ref()
                .and_then(|list| list.rows.iter().find(|row| row.row.peer == peer))
                .and_then(|row| row.position.as_ref())
                .map(|pin| &pin.open),
            NodeId::Tab(tab) => self
                .tabs
                .as_ref()
                .and_then(|tabs| tabs.items.iter().find(|t| t.id == tab))
                .map(|t| &t.action),
            NodeId::ChatBar => self.chat.as_ref().and_then(|chat| back(&chat.bar)),
            NodeId::TopBar => back(&self.top),
            NodeId::Compose => self.chat.as_ref().map(|chat| &chat.compose.send),
            NodeId::Mark(key) => self
                .bubble(NodeId::Bubble(key))
                .and_then(|node| node.mark.as_ref()?.activate.as_ref()),
            NodeId::Photo(key) => self
                .bubble(NodeId::Bubble(key))
                .and_then(|node| node.photo.as_ref())
                .map(|photo| &photo.open),
            NodeId::File(key, index) => self
                .bubble(NodeId::Bubble(key))
                .and_then(|node| node.files.get(index))
                .map(|file| &file.open),
            NodeId::Voice(key) => self
                .bubble(NodeId::Bubble(key))
                .and_then(|node| node.voice.as_ref())
                .and_then(|voice| voice.toggle.as_ref()),
            NodeId::Record => self.chat.as_ref().map(|chat| &chat.compose.record),
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
    // The pin says the position in the list (1224), the ring does not.
    let avatar = Avatar::of(row.initials.clone(), peer, row.icon.as_ref(), None);
    let position = PositionBadge::of(peer, &row.name, &row.liveness, now);
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
    let draft = row
        .draft
        .as_ref()
        .map(|line| Text::Key(StrId::DraftRow, Vec::from([Arg::Str(line.clone())])));
    let spoken = match (&draft, row.last.is_empty()) {
        (Some(draft), _) => Text::Join(Vec::from([Text::Raw(row.name.clone()), draft.clone()])),
        (None, true) => Text::Raw(row.name.clone()),
        (None, false) => Text::Raw(format!("{}, {}", row.name, row.last)),
    };
    let mut a11y = A11y::of(id, Role::Row, spoken);
    a11y.selected = selected;
    Row {
        item: ListItem {
            id,
            selected,
            activate: Action::of(Intent::OpenChat { peer: *peer }),
            menu,
            a11y,
        },
        avatar,
        live,
        position,
        draft,
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
        // The list's pin and the chat's bar open the map alike (1224).
        actions: Vec::from([Action::of(Intent::OpenMapAt {
            peer: Target::Peer(peer),
        })]),
        a11y: A11y::of(NodeId::ChatBar, Role::Bar, Text::Raw(name.clone())),
    };
    let bubbles = chat::bubbles(model, c);
    let places = chat::placements(&bubbles);
    let now = model.now;
    let mut groups: Vec<Group> = Vec::new();
    for (bubble, place) in bubbles.into_iter().zip(places) {
        let playing = model.playing
            == Some(super::voice::Playing {
                peer,
                key: bubble.key,
            });
        let node = bubble_node(peer, bubble, place, now, playing);
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
        compose: compose(model, c),
        a11y: A11y::of(NodeId::Chat, Role::Chat, Text::Raw(name)),
    }
}

/// Conversation `c`'s compose bar, its draft in the field (1219).
pub fn compose<B>(model: &UiModel<B>, c: usize) -> Compose {
    let conversation = model.conversations.get(c);
    let peer = conversation.map_or([0; 16], |c| c.peer);
    let draft = conversation.map(|c| &c.draft);
    Compose {
        id: NodeId::Compose,
        hint: Text::key(StrId::ComposeHint),
        text: draft.map(|d| d.text.clone()).unwrap_or_default(),
        reply: draft.and_then(|d| d.reply),
        send: Action::of(Intent::Send {
            to: Target::Peer(peer),
            body: None,
        }),
        record: Action::of(Intent::SendVoice {
            peer,
            pcm: Vec::new(),
            rate: 0,
        }),
        a11y: A11y::of(
            NodeId::Compose,
            Role::TextField,
            Text::key(StrId::ComposeHint),
        ),
    }
}

fn bubble_node(
    peer: [u8; 16],
    bubble: Bubble,
    place: Placement,
    now: Option<WallTime>,
    playing: bool,
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
    let photo = bubble.photo.as_ref().map(|photo| PhotoNode {
        id: NodeId::Photo(bubble.key),
        format: photo.format.clone(),
        bytes: photo.bytes,
        words: Text::Key(
            StrId::Photo,
            Vec::from([Arg::Str(super::photos::size_words(photo.bytes))]),
        ),
        open: Action::of(Intent::ViewPhoto {
            peer,
            key: bubble.key,
        }),
    });
    let files: Vec<FileNode> = bubble
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| FileNode {
            id: NodeId::File(bubble.key, index),
            name: super::files::safe_name(&file.name),
            bytes: file.bytes,
            size: super::photos::size_words(file.bytes),
            words: super::files::file_words(file),
            open: Action::of(Intent::OpenFile {
                peer,
                key: bubble.key,
                index,
            }),
        })
        .collect();
    let voice = bubble.voice.as_ref().map(|voice| VoiceNode {
        id: NodeId::Voice(bubble.key),
        mode: voice.mode,
        bytes: voice.bytes,
        duration_ms: voice.duration_ms,
        duration: super::voice::duration_words(voice.duration_ms),
        levels: voice.levels.clone(),
        playing,
        toggle: voice.playable().then(|| {
            let key = bubble.key;
            Action::of(match playing {
                true => Intent::PauseVoice { peer, key },
                false => Intent::PlayVoice { peer, key },
            })
        }),
        words: super::voice::voice_words(voice),
    });
    let spoken = match (&live, &photo, files.first(), &voice, bubble.body.is_empty()) {
        (Some(live), _, _, _, true) => live.clone(),
        (_, Some(photo), _, _, true) => photo.words.clone(),
        (_, _, Some(file), _, true) => file.words.clone(),
        (_, _, _, Some(voice), true) => voice.words.clone(),
        _ => Text::Raw(bubble.body.clone()),
    };
    let mark = bubble.mark.map(|mark| MarkNode {
        mark,
        words: mark_words(mark, bubble.read_at, now),
        activate: (mark == chat::Mark::Failed).then(|| {
            Action::of(Intent::SendAgain {
                peer,
                key: bubble.key,
            })
        }),
    });
    BubbleNode {
        id,
        a11y: A11y::of(id, Role::Bubble, spoken),
        bubble,
        place,
        live,
        mark,
        photo,
        files,
        voice,
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

impl ToJson for Avatar {
    fn to_json(&self) -> Value {
        json!({
            "initials": self.initials,
            "fg": format!("{:06x}", self.fg),
            "bg": format!("{:06x}", self.bg),
            "ring": opt(self.ring, |r| json!(format!("{r:?}"))),
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

impl PositionBadge {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "age_s": self.age_s,
            "liveness": self.liveness.word(),
            "tint": self.tint.word(),
            "open": self.open.to_json(),
            "a11y": self.a11y.to_json(),
        })
    }
}

impl Row {
    pub fn to_json(&self) -> Value {
        let r = &self.row;
        json!({
            "id": self.item.id.resource(),
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
            "position": opt(self.position.as_ref(), PositionBadge::to_json),
            "draft": opt(self.draft.as_ref(), Text::to_json),
            "selected": self.item.selected,
            "activate": self.item.activate.to_json(),
            "menu": actions(&self.item.menu),
            "a11y": self.item.a11y.to_json(),
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
            "mark_node": opt(self.mark.as_ref(), |m| json!({
                "mark": m.mark.word(),
                "resource": m.mark.resource(),
                "words": m.words.to_json(),
                "activate": opt(m.activate.as_ref(), Action::to_json),
            })),
            "photo": opt(self.photo.as_ref(), |p| json!({
                "id": p.id.resource(),
                "format": p.format,
                "bytes": p.bytes,
                "words": p.words.to_json(),
                "open": p.open.to_json(),
            })),
            "files": self.files.iter().map(|f| json!({
                "id": f.id.resource(),
                "name": f.name,
                "bytes": f.bytes,
                "size": f.size,
                "words": f.words.to_json(),
                "open": f.open.to_json(),
            })).collect::<Vec<_>>(),
            "voice": opt(self.voice.as_ref(), |v| json!({
                "id": v.id.resource(),
                "mode": v.mode,
                "bytes": v.bytes,
                "duration_ms": v.duration_ms,
                "duration": v.duration,
                "levels": v.levels,
                "playing": v.playing,
                "toggle": opt(v.toggle.as_ref(), Action::to_json),
                "words": v.words.to_json(),
            })),
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
                "text": self.compose.text,
                "reply": opt(self.compose.reply, |id| hex(&id)),
                "send": self.compose.send.to_json(),
                "record": self.compose.record.to_json(),
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
        map.insert("tabs".into(), opt(self.tabs.as_ref(), Tabs::to_json));
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

    /// 1224: the row's map pin is there for a fix younger than a day and
    /// gone for an older one, tinted by the stream's state, its tap the
    /// map on the peer, its label the catalogue's in either language.
    #[test]
    fn a_fix_of_23_hours_has_a_pin_and_one_of_25_hours_none() {
        const T: i64 = 1_800_000_000;
        let peer = [0x42; 16];
        let one = Liveness::of(&[T], None);
        let at = |hours: i64| PositionBadge::of(&peer, "Anna", &one, Some(T + hours * 3600));
        let pin = at(23).expect("a pin at 23 h");
        assert_eq!(pin.id, NodeId::Pin(peer));
        assert_eq!(pin.id.resource(), format!("pin_{}", to_hex(&peer)));
        assert_eq!(pin.age_s, Some(23 * 3600));
        assert_eq!(pin.tint, Ring::Secondary);
        assert_eq!(
            pin.open.intent,
            Intent::OpenMapAt {
                peer: Target::Peer(peer)
            }
        );
        assert_eq!(pin.a11y.role, Role::Button);
        assert_eq!(
            pin.a11y.label.resolve(Lang::En),
            "Position of Anna, 23 h ago"
        );
        assert_eq!(
            pin.a11y.label.resolve(Lang::De),
            "Position von Anna, vor 23 Std."
        );
        assert!(at(25).is_none(), "older than a day");
        assert!(at(24).is_none(), "a day is the edge");
        // Live is the accent; no fix is no pin; without a clock the
        // stream's state decides.
        let live = Liveness::of(&[T], Some(T + 60));
        let pin = PositionBadge::of(&peer, "Anna", &live, Some(T + 60)).expect("live");
        assert_eq!(pin.tint, Ring::Accent);
        assert!(PositionBadge::of(&peer, "Anna", &Liveness::default(), Some(T)).is_none());
        let unclocked = PositionBadge::of(&peer, "Anna", &one, None).expect("recent");
        assert_eq!(unclocked.age_s, None);
        // A stream of reports a day apart is live for two; its pin goes
        // after one all the same.
        let slow = Liveness::of(
            &[T, T + 86_400, T + 2 * 86_400],
            Some(T + 3 * 86_400 + 3600),
        );
        assert_eq!(slow.state, liveness::State::Live);
        assert!(PositionBadge::of(&peer, "Anna", &slow, Some(T + 3 * 86_400 + 3600)).is_none());
    }

    /// The chat's bar opens the map as the list's pin does (1224); a row
    /// without a position has no pin and no tap on one.
    #[test]
    fn the_chat_bar_opens_the_map_as_the_pin_does() {
        let mut model = model_with(&[(false, 1_793_350_800)]);
        let peer = [0x42; 16];
        model.selected = 0;
        let wide = model.scene(LayoutClass::Wide);
        let bar = wide
            .chat
            .as_ref()
            .map(|chat| chat.bar.actions[0].intent.clone());
        assert_eq!(
            bar,
            Some(Intent::OpenMapAt {
                peer: Target::Peer(peer)
            })
        );
        let row = wide.list.as_ref().map(|list| &list.rows[0]);
        assert!(row.is_some_and(|row| row.position.is_none() && row.avatar.ring.is_none()));
        assert!(wide.action(NodeId::Pin(peer)).is_none());
    }

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

    /// The ladder in the scene (1232): each state's mark, its words in
    /// both languages, and only the failed one's tap, which sends it again.
    #[test]
    fn each_state_has_its_mark_its_words_and_only_failed_a_tap() {
        let t = 1_793_350_800;
        let states = [
            DeliveryState::Queued,
            DeliveryState::Sent,
            DeliveryState::InMailbox,
            DeliveryState::Delivered,
            DeliveryState::Read {
                at: (t + 600) as u32,
            },
            DeliveryState::Failed,
        ];
        let bodies: Vec<(bool, i64)> = (0..states.len())
            .map(|i| (true, t + 600 * i as i64))
            .collect();
        let mut model = model_with(&bodies);
        for (message, state) in model.conversations[0].messages.iter_mut().zip(states) {
            message.delivery = Some(state);
        }
        model.selected = 0;
        let peer = [0x42; 16];
        let scene = model.scene(LayoutClass::Compact);
        let marks: Vec<&MarkNode> = scene
            .chat
            .as_ref()
            .expect("the chat")
            .groups
            .iter()
            .flat_map(|g| &g.bubbles)
            .map(|b| b.mark.as_ref().expect("ours has a mark"))
            .collect();
        let got: Vec<(chat::Mark, String, String)> = marks
            .iter()
            .map(|m| (m.mark, m.words.resolve(Lang::En), m.words.resolve(Lang::De)))
            .collect();
        let want = [
            (chat::Mark::Sending, "Sending", "Wird gesendet"),
            (
                chat::Mark::Unconfirmed,
                "Sent, not confirmed yet",
                "Gesendet, noch nicht bestätigt",
            ),
            (
                chat::Mark::Mailbox,
                "Waiting in the mailbox",
                "Wartet im Postfach",
            ),
            (chat::Mark::Delivered, "Delivered", "Zugestellt"),
            // 10:10 at the clock's +1 h.
            (chat::Mark::Read, "Read at 10:10", "Gelesen um 10:10"),
            (
                chat::Mark::Failed,
                "Not sent. Double tap to send again",
                "Nicht gesendet. Doppeltippen, um es erneut zu senden",
            ),
        ];
        let want: Vec<(chat::Mark, String, String)> = want
            .iter()
            .map(|(m, en, de)| (*m, en.to_string(), de.to_string()))
            .collect();
        assert_eq!(got, want);
        for (i, mark) in marks.iter().enumerate() {
            let key = i as u64 + 1;
            let again = Intent::SendAgain { peer, key };
            match mark.mark {
                chat::Mark::Failed => {
                    assert_eq!(mark.activate.as_ref().map(|a| &a.intent), Some(&again));
                    assert_eq!(
                        scene.action(NodeId::Mark(key)).map(|a| &a.intent),
                        Some(&again)
                    );
                }
                _ => {
                    assert!(mark.activate.is_none(), "{:?}", mark.mark);
                    assert!(scene.action(NodeId::Mark(key)).is_none());
                }
            }
        }
        // The JSON names the rung's resource and its words.
        let json = scene.chat.as_ref().expect("the chat").to_json().to_string();
        assert!(json.contains(r#""resource":"state_ticks_read""#), "{json}");
        assert!(json.contains(r#""key":"state_mailbox""#), "{json}");
        // On a client of a home node the home took it: the mailbox's tick.
        model.home_client = true;
        let home = model.scene(LayoutClass::Compact);
        let second = home.chat.as_ref().and_then(|c| {
            c.groups
                .iter()
                .flat_map(|g| &g.bubbles)
                .nth(1)
                .and_then(|b| b.mark.clone())
        });
        assert_eq!(second.map(|m| m.mark), Some(chat::Mark::Mailbox));
    }

    /// The red mark's tap: the failed message leaves, the same text goes
    /// again as a new one at the end; on anything not failed it does
    /// nothing.
    #[test]
    fn send_again_replaces_the_failed_message_with_a_new_send() {
        use crate::ui::UiEffect;
        let t = 1_793_350_800;
        let mut model = model_with(&[(true, t), (true, t + 600)]);
        let peer = [0x42; 16];
        model.conversations[0].messages[0].delivery = Some(DeliveryState::Failed);
        let delivered = super::super::update(&mut model, UiEvent::SendAgain { peer, key: 2 });
        assert!(delivered.is_empty(), "a delivered one is not sent again");
        let effects = super::super::update(&mut model, UiEvent::SendAgain { peer, key: 1 });
        assert!(
            matches!(effects.as_slice(), [UiEffect::Send { to, body, .. }] if *to == peer && body == "m0"),
            "{effects:?}"
        );
        let bodies: Vec<&str> = model.conversations[0]
            .messages
            .iter()
            .map(|m| m.body.as_str())
            .collect();
        assert_eq!(bodies, ["m1", "m0"]);
        assert_eq!(
            model.conversations[0].messages[1].delivery,
            Some(DeliveryState::Queued)
        );
        assert!(super::super::update(&mut model, UiEvent::SendAgain { peer, key: 1 }).is_empty());
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

    /// A photo is a node of its bubble (1220): its words, a resource the
    /// walk finds, a tap that asks the shell to show it whole; a text
    /// without one has none, and the tap on it does nothing.
    #[test]
    fn a_photo_has_its_node_its_words_and_a_tap_that_shows_it() {
        use crate::ui::photos::{photo_fields, PhotoSize};
        use crate::ui::UiEffect;
        let t = 1_793_350_800;
        let mut model = model_with(&[(false, t), (false, t + 60)]);
        let peer = [0x42; 16];
        model.conversations[0].messages[1].body = String::new();
        model.conversations[0].messages[1].fields = photo_fields("jpg", alloc::vec![7; 312_400]);
        model.selected = 0;
        let scene = model.scene(LayoutClass::Compact);
        assert!(scene
            .bubble(NodeId::Bubble(1))
            .expect("text")
            .photo
            .is_none());
        let node = scene.bubble(NodeId::Bubble(2)).expect("the photo's bubble");
        let photo = node.photo.as_ref().expect("a photo node");
        assert_eq!(photo.id.resource(), "photo_2");
        assert_eq!((photo.format.as_str(), photo.bytes), ("jpg", 312_400));
        assert_eq!(photo.words.resolve(Lang::En), "Photo, 313 KB");
        assert_eq!(photo.words.resolve(Lang::De), "Foto, 313 KB");
        assert_eq!(node.a11y.label, photo.words, "the screen reader says it");
        let json = node.to_json();
        assert_eq!(json["photo"]["id"], "photo_2");
        assert_eq!(json["photo"]["bytes"], 312_400);
        assert_eq!(
            scene.action(NodeId::Photo(2)).map(|a| a.intent.clone()),
            Some(Intent::ViewPhoto { peer, key: 2 })
        );
        assert_eq!(scene.action(NodeId::Photo(1)), None);
        let shown = super::super::update(&mut model, UiEvent::ViewPhoto { peer, key: 2 });
        assert_eq!(shown, alloc::vec![UiEffect::ShowPhoto { peer, key: 2 }]);
        assert!(super::super::update(&mut model, UiEvent::ViewPhoto { peer, key: 1 }).is_empty());
        assert_eq!(chat::preview(&model.conversations[0].messages[1]), "photo");
        assert_eq!(chat::kind(&model.conversations[0].messages[1]), "photo");

        // Sending one: a message of its own, the image field alone, the
        // view on it.
        let sent = super::super::update(
            &mut model,
            UiEvent::SendPhoto {
                peer,
                format: "jpg".into(),
                data: alloc::vec![0xff, 0xd8, 0xff],
            },
        );
        let fields = photo_fields("jpg", alloc::vec![0xff, 0xd8, 0xff]);
        // The read-receipt mark rides on every send (1230), after it.
        assert!(
            matches!(sent.as_slice(), [UiEffect::Send { to, body, fields: f }] if *to == peer && body.is_empty() && f.starts_with(&fields)),
            "{sent:?}"
        );
        let last = model.conversations[0].messages.len() - 1;
        assert_eq!(model.message_cursor, last);
        assert!(model.conversations[0].messages[last].outbound);
        assert!(super::super::update(
            &mut model,
            UiEvent::SendPhoto {
                peer,
                format: "jpg".into(),
                data: Vec::new()
            }
        )
        .is_empty());

        // The setting: kept in the model, remembered in the config.
        assert_eq!(model.photo_size, PhotoSize::Phone);
        let remembered = super::super::update(&mut model, UiEvent::PhotoSize(PhotoSize::Small));
        assert_eq!(model.photo_size, PhotoSize::Small);
        assert_eq!(
            remembered,
            alloc::vec![UiEffect::Remember {
                key: "photo_size",
                value: "small"
            }]
        );
    }

    /// A file is a row of its bubble (1221): its name, size and words, a
    /// resource the walk finds, a tap that asks the shell to save and open
    /// it; a file too large for a receiver is refused before it is a
    /// message.
    #[test]
    fn a_file_has_its_row_its_words_and_a_tap_that_opens_it() {
        use crate::ui::files::{file_fields, LARGEST};
        use crate::ui::UiEffect;
        let t = 1_793_350_800;
        let mut model = model_with(&[(false, t), (false, t + 60)]);
        let peer = [0x42; 16];
        model.conversations[0].messages[1].body = String::new();
        model.conversations[0].messages[1].fields =
            file_fields("report.pdf", alloc::vec![7; 312_400]);
        model.selected = 0;
        let scene = model.scene(LayoutClass::Compact);
        assert!(scene
            .bubble(NodeId::Bubble(1))
            .expect("text")
            .files
            .is_empty());
        let node = scene.bubble(NodeId::Bubble(2)).expect("the file's bubble");
        let file = node.files.first().expect("a file row");
        assert_eq!(file.id.resource(), "file_2_0");
        assert_eq!(
            (file.name.as_str(), file.bytes, file.size.as_str()),
            ("report.pdf", 312_400, "313 KB")
        );
        assert_eq!(file.words.resolve(Lang::En), "File report.pdf, 313 KB");
        assert_eq!(file.words.resolve(Lang::De), "Datei report.pdf, 313 KB");
        assert_eq!(node.a11y.label, file.words, "the screen reader says it");
        let json = node.to_json();
        assert_eq!(json["files"][0]["id"], "file_2_0");
        assert_eq!(json["files"][0]["name"], "report.pdf");
        assert_eq!(json["files"][0]["bytes"], 312_400);
        assert_eq!(
            scene.action(NodeId::File(2, 0)).map(|a| a.intent.clone()),
            Some(Intent::OpenFile {
                peer,
                key: 2,
                index: 0
            })
        );
        assert_eq!(scene.action(NodeId::File(2, 1)), None);
        assert_eq!(scene.action(NodeId::File(1, 0)), None);
        let opened = super::super::update(
            &mut model,
            UiEvent::OpenFile {
                peer,
                key: 2,
                index: 0,
            },
        );
        assert_eq!(
            opened,
            alloc::vec![UiEffect::OpenFile {
                peer,
                key: 2,
                index: 0
            }]
        );
        assert!(super::super::update(
            &mut model,
            UiEvent::OpenFile {
                peer,
                key: 2,
                index: 1
            }
        )
        .is_empty());
        assert!(super::super::update(
            &mut model,
            UiEvent::OpenFile {
                peer,
                key: 1,
                index: 0
            }
        )
        .is_empty());
        assert_eq!(
            chat::preview(&model.conversations[0].messages[1]),
            "report.pdf"
        );
        assert_eq!(chat::kind(&model.conversations[0].messages[1]), "file");

        // Sending one: a message of its own, the attachments field alone,
        // the view on it.
        let sent = super::super::update(
            &mut model,
            UiEvent::SendFile {
                peer,
                name: "/sdcard/Download/notes.txt".into(),
                data: b"hello".to_vec(),
            },
        );
        let fields = file_fields("notes.txt", b"hello".to_vec());
        assert!(
            matches!(sent.as_slice(), [UiEffect::Send { to, body, fields: f }] if *to == peer && body.is_empty() && f.starts_with(&fields)),
            "{sent:?}"
        );
        let last = model.conversations[0].messages.len() - 1;
        assert_eq!(model.message_cursor, last);
        assert!(model.conversations[0].messages[last].outbound);

        // Too large: nothing goes, nothing is added, the toast says why.
        let before = model.conversations[0].messages.len();
        let refused = super::super::update(
            &mut model,
            UiEvent::SendFile {
                peer,
                name: "big.iso".into(),
                data: alloc::vec![0; LARGEST + 1],
            },
        );
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(model.conversations[0].messages.len(), before);
        let toast = model.current_toast().expect("a toast").text.clone();
        assert_eq!(
            toast,
            "This file is 991 KB. A message carries at most 990 KB."
        );
    }

    /// A voice message (1222): its node with length, level bar and words,
    /// the play tap that turns into pause while it plays and back at its
    /// end, a mode this build does not play with no tap, the list's words,
    /// the compose bar's microphone, and a send that carries the audio
    /// field alone.
    #[test]
    fn a_voice_message_has_its_player_its_words_and_the_microphone_sends_one() {
        use crate::ui::voice::{voice_fields, AM_CODEC2_2400, AM_CODEC2_700C, AM_OPUS_OGG};
        use crate::ui::UiEffect;
        let t = 1_793_350_800;
        let mut model = model_with(&[(false, t), (false, t + 60), (false, t + 120)]);
        let peer = [0x42; 16];
        // Two seconds of Codec2 2400 as Sideband sends it, and 700C, which
        // this build does not play.
        model.conversations[0].messages[1].body = String::new();
        model.conversations[0].messages[1].fields =
            voice_fields(AM_CODEC2_2400, alloc::vec![0; 600]);
        model.conversations[0].messages[2].fields =
            voice_fields(AM_CODEC2_700C, alloc::vec![0; 200]);
        model.selected = 0;
        let scene = model.scene(LayoutClass::Compact);
        assert!(scene
            .bubble(NodeId::Bubble(1))
            .expect("text")
            .voice
            .is_none());
        let node = scene.bubble(NodeId::Bubble(2)).expect("the voice bubble");
        let voice = node.voice.as_ref().expect("a voice node");
        assert_eq!(voice.id.resource(), "voice_2");
        assert_eq!(
            (
                voice.mode,
                voice.bytes,
                voice.duration_ms,
                voice.duration.as_str(),
                voice.playing
            ),
            (AM_CODEC2_2400, 600, 2_000, "0:02", false)
        );
        assert!(voice.levels.is_empty(), "Codec2 frames are all one size");
        assert_eq!(voice.words.resolve(Lang::En), "Voice message, 0:02");
        assert_eq!(voice.words.resolve(Lang::De), "Sprachnachricht, 0:02");
        assert_eq!(node.a11y.label, voice.words, "the screen reader says it");
        let json = node.to_json();
        assert_eq!(json["voice"]["id"], "voice_2");
        assert_eq!(json["voice"]["duration"], "0:02");
        assert_eq!(json["voice"]["mode"], 8);
        let play = Intent::PlayVoice { peer, key: 2 };
        assert_eq!(
            scene.action(NodeId::Voice(2)).map(|a| a.intent.clone()),
            Some(play.clone())
        );
        assert_eq!(scene.action(NodeId::Voice(1)), None);
        let old = scene.bubble(NodeId::Bubble(3)).expect("the 700C bubble");
        let old = old.voice.as_ref().expect("still a voice node");
        assert_eq!(old.toggle, None);
        assert_eq!(
            old.words.resolve(Lang::En),
            "Voice message, 0:02, which cannot be played here"
        );
        assert_eq!(
            chat::preview(&model.conversations[0].messages[1]),
            "voice message 0:02"
        );
        assert_eq!(chat::kind(&model.conversations[0].messages[1]), "voice");

        // A tap plays it, the bubble shows pause; pause stops it; the
        // player's end puts play back; the 700C one does not start.
        let played = super::super::update(&mut model, UiEvent::PlayVoice { peer, key: 2 });
        assert_eq!(played, alloc::vec![UiEffect::PlayVoice { peer, key: 2 }]);
        let scene = model.scene(LayoutClass::Compact);
        let voice = scene
            .bubble(NodeId::Bubble(2))
            .and_then(|n| n.voice.clone())
            .expect("voice");
        assert!(voice.playing);
        assert_eq!(
            voice.toggle.map(|a| a.intent),
            Some(Intent::PauseVoice { peer, key: 2 })
        );
        let paused = super::super::update(&mut model, UiEvent::PauseVoice { peer, key: 2 });
        assert_eq!(paused, alloc::vec![UiEffect::PauseVoice { peer, key: 2 }]);
        assert_eq!(model.playing, None);
        super::super::update(&mut model, UiEvent::PlayVoice { peer, key: 2 });
        super::super::update(&mut model, UiEvent::VoiceEnded { peer, key: 2 });
        assert_eq!(model.playing, None);
        assert!(super::super::update(&mut model, UiEvent::PlayVoice { peer, key: 3 }).is_empty());
        assert!(super::super::update(&mut model, UiEvent::PlayVoice { peer, key: 1 }).is_empty());

        // The microphone is the compose bar's, its words the hint to hold.
        let compose = &model
            .scene(LayoutClass::Compact)
            .chat
            .expect("chat")
            .compose;
        assert_eq!(
            compose.record.intent,
            Intent::SendVoice {
                peer,
                pcm: Vec::new(),
                rate: 0
            }
        );
        assert_eq!(
            Intent::SendVoice {
                peer,
                pcm: Vec::new(),
                rate: 0
            }
            .label()
            .text(Lang::En),
            "Hold to record a voice message"
        );

        // Sending one: a message of its own, the audio field alone, the
        // view on it.
        let sent = super::super::update(
            &mut model,
            UiEvent::SendVoice {
                peer,
                data: b"OggS".to_vec(),
            },
        );
        let fields = voice_fields(AM_OPUS_OGG, b"OggS".to_vec());
        assert!(
            matches!(sent.as_slice(), [UiEffect::Send { to, body, fields: f }] if *to == peer && body.is_empty() && f.starts_with(&fields)),
            "{sent:?}"
        );
        let last = model.conversations[0].messages.len() - 1;
        assert_eq!(model.message_cursor, last);
    }
}
