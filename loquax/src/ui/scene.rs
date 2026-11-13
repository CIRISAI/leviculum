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
use super::compose::{self, Button, Round, SheetRow};
use super::counters::Op;
use super::intent::{Intent, Messages, Switch, Target};
use super::liveness::{self, Liveness};
use super::recording::{self, Fold, LevelTrack, Phase, Verb};
use super::strings::{Arg, StrId, Text};
use super::voice::BARS;
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
    /// lnomad's browser, hosted (1337): on the phone and the M9 only,
    /// where [`super::browse::Browse::enabled`] says so.
    Browse,
    /// Reticulum Relay Chat's rooms on a hub (1379), where
    /// [`super::rooms::Rooms::enabled`] says so.
    Rooms,
    /// Explore (1578): the pages and the public rooms under one button,
    /// the bar's third and last. Browse and Rooms are no buttons of the
    /// bar any more: they are what Explore opens.
    Explore,
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
    /// A bubble's bytes under way, by its message's key (1662).
    Progress(u64),
    Compose,
    /// The compose bar's microphone (1222).
    Record,
    /// The compose bar's other buttons (1270): the emoji, the position
    /// button, the camera, and the round button outside the field as the
    /// plus or as Send.
    Emoji,
    Locate,
    Camera,
    Plus,
    Send,
    /// The photo staged in the compose bar (1270).
    Staged,
    /// The recording bar (1276) and its parts: the timer, the live
    /// waveform, the slide-to-cancel hint, the line under the waveform
    /// (nothing heard, the cap), and a button or the lock chip by its
    /// verb. The microphone held keeps [`NodeId::Record`].
    RecBar,
    RecTimer,
    RecWave,
    RecCancel,
    RecNotice,
    Rec(Verb),
    Sheet,
    /// A row of the sheet the compose bar opened (1270).
    SheetRow(SheetRow),
    Card,
    Chip(u32),
    /// The chip at the top of a chat while its position share runs (1273),
    /// and its Stop.
    ShareChip,
    ShareStop,
    /// A running share in Settings, Privacy, by its peer (1273); its Stop.
    ShareRow([u8; 16]),
    ShareRowStop([u8; 16]),
    /// Group chats (1307): "New group" and a friend's row in its picker,
    /// an invitation's card by its group, the member sheet and a member's
    /// row in it.
    NewGroup,
    PickRow([u8; 16]),
    Join([u8; 16]),
    Members,
    Member([u8; 16]),
    /// The reader's avatar in the top bar, which opens the profile, and
    /// the profile and its "Share my address" (1443).
    Me,
    Profile,
    ProfileShare,
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
            NodeId::Tab(TabId::Browse) => "tab_browse".to_string(),
            NodeId::Tab(TabId::Rooms) => "tab_rooms".to_string(),
            NodeId::Tab(TabId::Explore) => "tab_explore".to_string(),
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
            NodeId::Progress(key) => format!("progress_{key}"),
            NodeId::Compose => "compose".to_string(),
            NodeId::Record => "record".to_string(),
            NodeId::Emoji => "emoji".to_string(),
            NodeId::Locate => "locate".to_string(),
            NodeId::Camera => "camera".to_string(),
            NodeId::Plus => "plus".to_string(),
            NodeId::Send => "send".to_string(),
            NodeId::Staged => "staged".to_string(),
            NodeId::RecBar => "rec_bar".to_string(),
            NodeId::RecTimer => "rec_timer".to_string(),
            NodeId::RecWave => "rec_wave".to_string(),
            NodeId::RecCancel => "rec_cancel".to_string(),
            NodeId::RecNotice => "rec_notice".to_string(),
            NodeId::Rec(verb) => verb.word().to_string(),
            NodeId::Sheet => "sheet".to_string(),
            NodeId::SheetRow(row) => format!("sheet_{}", row.word()),
            NodeId::Card => "card".to_string(),
            NodeId::Chip(n) => format!("chip_{n}"),
            NodeId::ShareChip => "share_chip".to_string(),
            NodeId::ShareStop => "share_stop".to_string(),
            NodeId::ShareRow(peer) => format!("share_{}", to_hex(peer)),
            NodeId::ShareRowStop(peer) => format!("share_stop_{}", to_hex(peer)),
            NodeId::NewGroup => "new_group".to_string(),
            NodeId::PickRow(peer) => format!("pick_{}", to_hex(peer)),
            NodeId::Join(group) => format!("join_{}", to_hex(group)),
            NodeId::Members => "members".to_string(),
            NodeId::Member(peer) => format!("member_{}", to_hex(peer)),
            NodeId::Me => "me".to_string(),
            NodeId::Profile => "profile".to_string(),
            NodeId::ProfileShare => "profile_share".to_string(),
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
    pub(super) fn of(
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
    /// A share of our position runs in this chat (1273): the row shows a
    /// small ⌖ in the accent colour on our side.
    pub sharing: bool,
    /// A group's row (1307): its avatar is the member count, its line
    /// "Invitation" until the Join.
    pub group: Option<super::group_scene::GroupBadge>,
    /// "Two contacts are called Lew" (1452): drawn quietly beside the name
    /// while another peer shares it; both rows carry their hash tail then.
    pub same_name: Option<Text>,
}

/// A row is its list item: `row.selected`, `row.activate`, `row.a11y`
/// read the shared part as they read the row's own fields before 1240.
impl core::ops::Deref for Row {
    type Target = ListItem;

    fn deref(&self) -> &ListItem {
        &self.item
    }
}

/// The conversation list: the window of rows the viewport asked for
/// (1490), inside the whole order.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    pub id: NodeId,
    pub rows: Vec<Row>,
    /// The first row's place in the whole order, and the order's length:
    /// the scroll geometry when `rows` is a window of it.
    pub first: usize,
    pub total: usize,
    /// The first row on screen, in the whole order, of a list apart
    /// (1546, [`super::Viewing::apart`]): where the reader's cursor keeps
    /// its line while the rows move around it. `None` where the frontend
    /// scrolls the list itself.
    pub top: Option<usize>,
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
    /// The words the bubble reads (1268): its body, none when the body
    /// holds nothing to read. A bubble is one shape whatever it carries:
    /// its media first (the photo, the file rows, the voice player), these
    /// words under them, the time and the mark closing the last row. A
    /// renderer draws no text row where this is none, so a photo without
    /// words is the photo and its time in one bubble.
    pub text: Option<String>,
    /// The newest report's words (`live · every 10 min · 2 min ago`).
    pub live: Option<Text>,
    /// The sentence of the position it carries, [`position_words`]: a
    /// renderer draws it after the pin, never the coordinates.
    pub position: Option<Text>,
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
    /// The invitation it carries, as a card (1307).
    pub invite: Option<super::group_scene::InviteNode>,
    /// Its bytes under way (1662): a bar and its words, "wird gesendet ·
    /// 43 %", under the media. Boxed, as the bubble's transfer is.
    pub progress: Option<Box<ProgressNode>>,
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
    /// Its bytes on the wire, [`super::photos::PhotoRef::bytes`].
    pub bytes: u32,
    /// `Photo, 312 KB`.
    pub words: Text,
    /// [`Intent::ViewPhoto`].
    pub open: Action,
    /// A picture sent as a file (1662): its place among the bubble's
    /// files, whose line is drawn under the thumbnail. A target that
    /// draws no thumbnail (the window, the terminal, the M9) draws the
    /// file's line alone.
    pub file: Option<u16>,
}

/// A bubble's bytes under way (1662): how far, for the bar, and the
/// words beside it, which a screen reader says too.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressNode {
    pub id: NodeId,
    /// 0 to 100.
    pub percent: u8,
    /// "wird gesendet · 43 %", "wird geholt · 1,2 MB von 3,0 MB".
    pub words: Text,
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
///
/// `witness` says who confirmed [`chat::Mark::Arrived`] when it was not
/// a proof (1395, 1462): the same one tick, the words of the propagation
/// network.
pub fn mark_words(
    mark: chat::Mark,
    read_at: Option<u32>,
    witness: Option<super::arrival::Witness>,
    now: Option<WallTime>,
) -> Text {
    use super::arrival::Witness;
    match mark {
        chat::Mark::Sending => Text::key(StrId::StateSending),
        chat::Mark::Unconfirmed => Text::key(StrId::StateUnconfirmed),
        chat::Mark::Mailbox => Text::key(StrId::StateMailbox),
        chat::Mark::Arrived => match witness {
            Some(Witness::Collected) => Text::key(StrId::StateCollected),
            _ => Text::key(StrId::StateArrived),
        },
        chat::Mark::Delivered => Text::key(StrId::StateDelivered),
        // A read without its time (an import's, 1506) says "Read" alone:
        // 0 is no time, never the epoch.
        chat::Mark::Read => match read_at.filter(|&at| at > 0) {
            Some(at) => {
                let at = WallTime {
                    unix_secs: i64::from(at),
                    offset_secs: now.map_or(0, |now| now.offset_secs),
                };
                Text::Key(StrId::StateRead, Vec::from([Arg::Str(at.hh_mm())]))
            }
            None => Text::key(StrId::StateReadUntimed),
        },
        chat::Mark::Failed => Text::key(StrId::StateFailed),
    }
}

/// Bubbles of one sender, minutes apart, on one day ([`chat::joins`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: NodeId,
    pub ours: bool,
    /// Who wrote: the reader, or the peer's name; in a group's chat the
    /// writer's (1307).
    pub sender: Text,
    /// The renderer draws [`Group::sender`] above the first bubble: a
    /// group's chat, a group of bubbles that is not the reader's.
    pub named: bool,
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
    /// The round button outside the field (1270): the plus while there
    /// is nothing to send, Send once there is ([`Round::of`]).
    pub round: Round,
    /// The bar's buttons in the order drawn (1270,
    /// [`compose::buttons`]): those inside the field, then the round
    /// one. What the shell cannot do is not among them.
    pub buttons: Vec<ComposeButton>,
    /// The photo the camera took, waiting for Send and its caption.
    pub staged: Option<StagedNode>,
    /// While a voice message records for this chat (1276): the bar the
    /// renderer draws instead of the field and its buttons.
    pub recording: Option<RecordingBar>,
    pub a11y: A11y,
}

/// The recording bar (1276, `docs/src/concepts/loquax-voice-recording.md`):
/// the compose bar while a voice message records, held, locked, paused
/// or stopped at the cap.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordingBar {
    pub id: NodeId,
    pub phase: Phase,
    /// The red dot pulses: it records and is heard, or not yet doubted.
    pub pulsing: bool,
    pub timer: RecTimer,
    pub wave: RecWave,
    /// "Slide left to cancel" while held, "Release to cancel" once past
    /// the distance.
    pub cancel: Option<RecHint>,
    /// The microphone, grown and red, while held: the finger is on it.
    pub held: Option<RecHint>,
    /// The lock chip above the button: open while held (a tap locks),
    /// closed once locked.
    pub lock: Option<LockChip>,
    /// The bin, pause or resume, play, Send, in the order drawn.
    pub buttons: Vec<RecButton>,
    /// The line under the waveform: nothing heard (with the way to the
    /// permission), or the cap.
    pub notice: Option<RecNotice>,
    pub a11y: A11y,
}

/// `REC 00:07`, amber in the last 30 s.
#[derive(Clone, Debug, PartialEq)]
pub struct RecTimer {
    pub id: NodeId,
    pub ms: u32,
    pub words: Text,
    pub amber: bool,
}

/// The waveform: the newest windows while it records, scrolling right to
/// left, the whole recording folded to the same width once it stands.
#[derive(Clone, Debug, PartialEq)]
pub struct RecWave {
    pub id: NodeId,
    /// Levels 0 to 100, oldest first: at most
    /// [`recording::LIVE_WINDOWS`].
    pub levels: Vec<u8>,
    /// It records: the newest bar is at the right edge and moves.
    pub moving: bool,
    pub a11y: A11y,
}

/// A part of the bar that says something and does nothing itself.
#[derive(Clone, Debug, PartialEq)]
pub struct RecHint {
    pub id: NodeId,
    pub words: Text,
    /// How far the finger is toward it, 0 to 100.
    pub progress: u8,
    /// Past it: a release does it.
    pub armed: bool,
}

/// The lock chip.
#[derive(Clone, Debug, PartialEq)]
pub struct LockChip {
    pub id: NodeId,
    pub closed: bool,
    /// How far the finger swiped up toward it, 0 to 100.
    pub progress: u8,
    /// [`Verb::Lock`] while open.
    pub action: Option<Action>,
    pub a11y: A11y,
}

/// A button of the bar.
#[derive(Clone, Debug, PartialEq)]
pub struct RecButton {
    pub id: NodeId,
    pub verb: Verb,
    pub action: Action,
    pub a11y: A11y,
}

/// The line under the waveform.
#[derive(Clone, Debug, PartialEq)]
pub struct RecNotice {
    pub id: NodeId,
    pub words: Text,
    /// The way to the microphone's permission when nothing is heard.
    pub action: Option<Action>,
}

/// `peer`'s recording bar, while one records for that chat.
pub fn recording_bar<B>(model: &UiModel<B>, peer: [u8; 16]) -> Option<RecordingBar> {
    let rec = &model.recording;
    if rec.peer != peer || !rec.phase.shown() {
        return None;
    }
    let verb = |verb: Verb| Action::of(Intent::Recording { peer, verb });
    let held = matches!(rec.phase, Phase::Held { .. });
    let levels = match rec.phase.running() {
        true => rec.track.live(recording::LIVE_WINDOWS).to_vec(),
        false => rec.track.bars(recording::LIVE_WINDOWS, Fold::Peak),
    };
    let verbs: &[Verb] = match (rec.phase, rec.previewing) {
        (Phase::Locked { .. }, _) => &[Verb::Discard, Verb::Pause, Verb::Send],
        (Phase::Paused, false) => &[Verb::Discard, Verb::Play, Verb::Resume, Verb::Send],
        (Phase::Paused, true) => &[Verb::Discard, Verb::StopPlay, Verb::Resume, Verb::Send],
        (Phase::Preview, false) => &[Verb::Discard, Verb::Play, Verb::Send],
        (Phase::Preview, true) => &[Verb::Discard, Verb::StopPlay, Verb::Send],
        _ => &[],
    };
    let notice = match (rec.silent(), rec.said) {
        (true, _) => Some(RecNotice {
            id: NodeId::RecNotice,
            words: Text::key(StrId::RecSilent),
            action: Some(verb(Verb::AskMicrophone)),
        }),
        (false, Some(recording::Said::Cap)) => Some(RecNotice {
            id: NodeId::RecNotice,
            words: Text::key(StrId::RecCap),
            action: None,
        }),
        _ => None,
    };
    let lock = match rec.phase {
        Phase::Held { .. } => Some(LockChip {
            id: NodeId::Rec(Verb::Lock),
            closed: false,
            progress: rec.lock_progress(),
            action: Some(verb(Verb::Lock)),
            a11y: A11y::of(
                NodeId::Rec(Verb::Lock),
                Role::Button,
                Text::key(StrId::RecLock),
            ),
        }),
        Phase::Locked { .. } => Some(LockChip {
            id: NodeId::Rec(Verb::Lock),
            closed: true,
            progress: 100,
            action: None,
            a11y: A11y::of(
                NodeId::Rec(Verb::Lock),
                Role::Chip,
                Text::key(StrId::RecLocked),
            ),
        }),
        _ => None,
    };
    let timer = rec.timer_words();
    Some(RecordingBar {
        id: NodeId::RecBar,
        phase: rec.phase,
        pulsing: rec.pulsing(),
        timer: RecTimer {
            id: NodeId::RecTimer,
            ms: rec.elapsed(),
            words: timer.clone(),
            amber: rec.amber(),
        },
        wave: RecWave {
            id: NodeId::RecWave,
            levels,
            moving: rec.phase.running(),
            a11y: A11y::of(NodeId::RecWave, Role::Bar, Text::key(StrId::RecWave)),
        },
        cancel: held.then(|| RecHint {
            id: NodeId::RecCancel,
            words: Text::key(match rec.armed() {
                true => StrId::RecReleaseCancel,
                false => StrId::VoiceCancel,
            }),
            progress: rec.cancel_progress(),
            armed: rec.armed(),
        }),
        held: held.then(|| RecHint {
            id: NodeId::Record,
            words: Text::key(StrId::RecHeld),
            progress: 0,
            armed: false,
        }),
        lock,
        buttons: verbs
            .iter()
            .map(|&v| RecButton {
                id: NodeId::Rec(v),
                verb: v,
                action: verb(v),
                a11y: A11y::of(NodeId::Rec(v), Role::Button, Text::key(v.label())),
            })
            .collect(),
        notice,
        a11y: A11y::of(NodeId::RecBar, Role::Bar, timer),
    })
}

impl RecordingBar {
    fn to_json(&self) -> Value {
        let hint = |h: &RecHint| {
            json!({
                "id": h.id.resource(),
                "words": h.words.to_json(),
                "progress": h.progress,
                "armed": h.armed,
            })
        };
        json!({
            "id": self.id.resource(),
            "phase": self.phase.word(),
            "pulsing": self.pulsing,
            "timer": {
                "id": self.timer.id.resource(),
                "ms": self.timer.ms,
                "words": self.timer.words.to_json(),
                "amber": self.timer.amber,
            },
            "wave": {
                "id": self.wave.id.resource(),
                "levels": self.wave.levels,
                "moving": self.wave.moving,
                "a11y": self.wave.a11y.to_json(),
            },
            "cancel": opt(self.cancel.as_ref(), hint),
            "held": opt(self.held.as_ref(), hint),
            "lock": opt(self.lock.as_ref(), |l| json!({
                "id": l.id.resource(),
                "closed": l.closed,
                "progress": l.progress,
                "action": opt(l.action.as_ref(), Action::to_json),
                "a11y": l.a11y.to_json(),
            })),
            "buttons": self.buttons.iter().map(|b| json!({
                "id": b.id.resource(),
                "action": b.action.to_json(),
                "a11y": b.a11y.to_json(),
            })).collect::<Vec<_>>(),
            "notice": opt(self.notice.as_ref(), |n| json!({
                "id": n.id.resource(),
                "words": n.words.to_json(),
                "action": opt(n.action.as_ref(), Action::to_json),
            })),
            "a11y": self.a11y.to_json(),
        })
    }

    /// The action of the part `id` names.
    pub fn action(&self, id: NodeId) -> Option<&Action> {
        if let Some(lock) = self.lock.as_ref().filter(|l| l.id == id) {
            return lock.action.as_ref();
        }
        if matches!(id, NodeId::RecNotice | NodeId::Rec(Verb::AskMicrophone)) {
            return self.notice.as_ref().and_then(|n| n.action.as_ref());
        }
        self.buttons.iter().find(|b| b.id == id).map(|b| &b.action)
    }
}

/// A button of the compose bar (1270).
#[derive(Clone, Debug, PartialEq)]
pub struct ComposeButton {
    pub id: NodeId,
    pub button: Button,
    pub action: Action,
    pub a11y: A11y,
}

/// The photo staged in the compose bar (1270): its size in words, its
/// cross.
#[derive(Clone, Debug, PartialEq)]
pub struct StagedNode {
    pub id: NodeId,
    pub format: String,
    pub bytes: usize,
    pub words: Text,
    pub remove: Action,
    pub a11y: A11y,
}

/// What the position sheet shows above its row (1270): the own fix for
/// its map preview, and its age, or the words that there is none.
#[derive(Clone, Debug, PartialEq)]
pub struct Whereabouts {
    /// Degrees times a million, north and east.
    pub fix: Option<(i32, i32)>,
    pub words: Text,
}

/// The chip at the top of a chat while our position share runs there
/// (1273): "Sharing your position, 42 min left" or "… until you stop",
/// and its Stop, one tap.
#[derive(Clone, Debug, PartialEq)]
pub struct ShareChip {
    pub id: NodeId,
    pub words: Text,
    pub stop: Action,
    pub a11y: A11y,
}

/// A running share as Settings, Privacy, "Position sharing" lists it
/// (1273): with whom, for how long, and its Stop.
#[derive(Clone, Debug, PartialEq)]
pub struct ShareRow {
    pub id: NodeId,
    pub peer: [u8; 16],
    pub name: String,
    pub words: Text,
    pub stop: Action,
    pub a11y: A11y,
}

/// The open conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Chat {
    pub id: NodeId,
    pub peer: [u8; 16],
    pub bar: TopBar,
    /// Our share's chip, while one runs here (1273).
    pub share: Option<ShareChip>,
    pub groups: Vec<Group>,
    /// A group's chat while the reader is only invited (1307): the card
    /// with Join above the compose bar.
    pub invite: Option<super::group_scene::InviteNode>,
    pub compose: Compose,
    /// A group its admin deleted (1307 pass four): the line drawn where the
    /// compose bar was, "Lew ended the group"; nothing can be written.
    pub ended: Option<Text>,
    /// Two peers of one name (1452, [`crate::names::Names::same_name`]):
    /// the sentence the bar's title ends with, apart, for a screen too
    /// narrow for both on one row; the M9 draws it on a row of its own
    /// under the bar (1587). Not in the JSON: the phone reads the title.
    pub same_name: Option<Text>,
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
    Chat {
        peer: [u8; 16],
    },
    /// The Browse tab in front (1337).
    Browse,
    /// The Rooms tab in front (1379).
    Rooms,
    /// The Explore tab's list in front (1578).
    Explore,
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
    /// The position sheet's fix and words (1270), while it is open.
    pub whereabouts: Option<Whereabouts>,
    pub card: Option<Card>,
    pub chips: Vec<Chip>,
    /// "New group", while it is open (1307).
    pub new_group: Option<super::group_scene::NewGroupNode>,
    /// A group's member sheet, while it is open (1307).
    pub members: Option<super::group_scene::MemberSheet>,
    /// The hosted browser's scene while the Browse tab is in front (1337):
    /// lnomad's own nodes (its bar with back, forward and reload, the
    /// address, the page as a window of runs, the places sheet, the name
    /// results, its toast), built by lnomad, never by Loquax.
    pub browse: Option<lnomad::app::scene::Scene>,
    /// The Rooms surface while its tab is in front (1379).
    pub rooms: Option<super::rooms::RoomsNode>,
    /// The Explore tab's list while it is in front (1578), boxed: inline
    /// it grew every scene by its size, and the M9's chats scene, which
    /// never holds it, went over its stack budget (108 360 of 100 000 B).
    pub explore: Option<alloc::boxed::Box<super::explore::ExploreNode>>,
    /// The profile over the screen while it is open (1443).
    pub profile: Option<super::profile::ProfileScreen>,
}

impl<B> UiModel<B> {
    /// The scene of the list and the open chat for `class`.
    pub fn scene(&self, class: LayoutClass) -> Scene {
        self.count(Op::Scenes, 1);
        let mut scene = self.content_scene(class);
        scene.profile = self.profile_screen();
        scene
    }

    /// The scene of the chats list alone (1463): [`UiModel::scene`] as a
    /// screen that shows the list without the open conversation sees it,
    /// with no chat, sheet or whereabouts, and none of them built. The
    /// M9's list screen: a chat of 4000 bubbles built and thrown away on
    /// every key was most of its draw.
    pub fn list_scene(&self, class: LayoutClass) -> Scene {
        self.count(Op::Scenes, 1);
        let mut scene = self.content_scene_of(class, false);
        scene.profile = self.profile_screen();
        scene
    }

    /// [`UiModel::scene`] without what lies over it.
    fn content_scene(&self, class: LayoutClass) -> Scene {
        self.content_scene_of(class, true)
    }

    /// The content scene, its chat built when `chat_open` and one is
    /// selected.
    fn content_scene_of(&self, class: LayoutClass, chat_open: bool) -> Scene {
        if let Some(scene) = self.tab_scene() {
            return self.counted(scene).in_class(class);
        }
        let open = self
            .conversations
            .get(self.selected)
            .filter(|_| chat_open)
            .map(|_| self.selected);
        let chat = open.map(|c| self.assembled_chat(c));
        let screen = match &chat {
            Some(chat) => ScreenId::Chat { peer: chat.peer },
            None => ScreenId::Chats,
        };
        let (sheet, whereabouts) = match &chat {
            Some(chat) => bar_sheet(self, chat.peer),
            None => (None, None),
        };
        self.counted(Scene {
            class: LayoutClass::Wide,
            screen,
            top: me_bar(self),
            tabs: self.viewing.tabs.then(|| tabs(self)),
            list: Some(self.assembled_list()),
            chat,
            sheet,
            whereabouts,
            card: None,
            chips: Vec::new(),
            new_group: super::group_scene::new_group(self),
            members: super::group_scene::member_sheet(self),
            browse: None,
            rooms: None,
            explore: None,
            profile: None,
        })
        .in_class(class)
    }

    /// The browser, a hub's rooms or Explore's list, whichever is in
    /// front, in a frame of its own: each built inline here put its own
    /// `Scene` temporaries on the stack of every scene, and the M9's chats
    /// scene, which builds none of them, went from 89 400 to 108 360 B
    /// (1578, the board's budget is 100 000).
    #[inline(never)]
    fn tab_scene(&self) -> Option<Scene> {
        if self.browse.enabled && self.browse.open {
            return Some(browse_scene(self));
        }
        if self.rooms.showing() {
            return Some(rooms_scene(self));
        }
        if super::explore::Explore::showing(self) {
            return Some(explore_scene(self));
        }
        None
    }

    /// The list the fold left for the viewport (1490), built by the
    /// first read after the fold and reused while its stamp is current
    /// (1591); a model changed outside `update` builds whole, as before.
    fn assembled_list(&self) -> List {
        self.built_list().unwrap_or_else(|| list(self))
    }

    /// The open chat the fold left for the viewport, as the list's;
    /// built whole otherwise.
    fn assembled_chat(&self, c: usize) -> Chat {
        self.built_chat(c).unwrap_or_else(|| chat(self, c))
    }

    /// `scene` with its nodes counted as built (1489), before a class
    /// throws any away: the reader's bar, the tabs, the list's rows, the
    /// chat's bar and its bubbles.
    fn counted(&self, scene: Scene) -> Scene {
        let tabs = scene.tabs.as_ref().map_or(0, |t| t.items.len());
        let rows = scene.list.as_ref().map_or(0, |l| l.rows.len());
        let chat = scene.chat.as_ref().map_or(0, |c| {
            1 + c.groups.iter().map(|g| g.bubbles.len()).sum::<usize>()
        });
        self.count(Op::SceneNodes, 1 + tabs + rows + chat);
        scene
    }
}

/// The Browse tab in front (1337): Loquax's bar titled by the page's
/// name (1621, walk 2: it said "Pages"), "Browse" on the start screen,
/// its tab bar with Browse active, and lnomad's scene, which is the whole
/// content. No list and no chat: the tab is a screen of its own in both
/// classes.
fn browse_scene<B>(model: &UiModel<B>) -> Scene {
    let mut top = me_bar(model);
    let title = match &model.browse.model.label {
        Some(name) => Text::Raw(name.clone()),
        None => Text::key(StrId::TabBrowse),
    };
    top.title = title.clone();
    top.actions = Vec::new();
    top.a11y = A11y::of(NodeId::TopBar, Role::Bar, title);
    Scene {
        class: LayoutClass::Wide,
        screen: ScreenId::Browse,
        top,
        tabs: model.viewing.tabs.then(|| tabs(model)),
        list: None,
        chat: None,
        sheet: None,
        whereabouts: None,
        card: None,
        chips: Vec::new(),
        new_group: None,
        members: None,
        browse: Some(model.browse.model.scene()),
        rooms: None,
        explore: None,
        profile: None,
    }
}

/// The Rooms tab in front (1379): Loquax's bar titled Rooms, its tab bar
/// with Rooms active, and the Rooms surface as the whole content. No list
/// of conversations and no chat: a room is not one.
fn rooms_scene<B>(model: &UiModel<B>) -> Scene {
    let mut top = me_bar(model);
    top.title = Text::key(StrId::TabRooms);
    top.actions = Vec::new();
    top.a11y = A11y::of(NodeId::TopBar, Role::Bar, Text::key(StrId::TabRooms));
    Scene {
        class: LayoutClass::Wide,
        screen: ScreenId::Rooms,
        top,
        tabs: model.viewing.tabs.then(|| tabs(model)),
        list: None,
        chat: None,
        sheet: None,
        whereabouts: None,
        card: None,
        chips: Vec::new(),
        new_group: None,
        members: None,
        browse: None,
        rooms: Some(super::rooms::node(model)),
        explore: None,
        profile: None,
    }
}

/// The Explore tab's list in front (1578): Loquax's bar titled Explore,
/// its tab bar with Explore active, and the two sections as the whole
/// content, in both classes.
fn explore_scene<B>(model: &UiModel<B>) -> Scene {
    let mut top = me_bar(model);
    top.title = Text::key(StrId::TabExplore);
    top.actions = Vec::new();
    top.a11y = A11y::of(NodeId::TopBar, Role::Bar, Text::key(StrId::TabExplore));
    Scene {
        class: LayoutClass::Wide,
        screen: ScreenId::Explore,
        top,
        tabs: model.viewing.tabs.then(|| tabs(model)),
        list: None,
        chat: None,
        sheet: None,
        whereabouts: None,
        card: None,
        chips: Vec::new(),
        new_group: None,
        members: None,
        browse: None,
        rooms: None,
        explore: Some(alloc::boxed::Box::new(super::explore::node(model))),
        profile: None,
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
            NodeId::Me => match &self.top.lead {
                Lead::Me { open, .. } => open.as_ref(),
                Lead::Back { .. } => None,
            },
            NodeId::Profile => self.profile.as_ref().map(|p| &p.back),
            NodeId::ProfileShare => self.profile.as_ref().map(|p| &p.share),
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
            NodeId::Emoji | NodeId::Locate | NodeId::Camera | NodeId::Plus | NodeId::Send => self
                .chat
                .as_ref()
                .and_then(|chat| chat.compose.buttons.iter().find(|b| b.id == id))
                .map(|b| &b.action),
            NodeId::Staged => self
                .chat
                .as_ref()
                .and_then(|chat| chat.compose.staged.as_ref())
                .map(|staged| &staged.remove),
            NodeId::Rec(_) | NodeId::RecNotice => self
                .chat
                .as_ref()
                .and_then(|chat| chat.compose.recording.as_ref())
                .and_then(|bar| bar.action(id)),
            NodeId::ShareChip | NodeId::ShareStop => self
                .chat
                .as_ref()
                .and_then(|chat| chat.share.as_ref())
                .map(|chip| &chip.stop),
            NodeId::PickRow(peer) => self
                .new_group
                .as_ref()
                .and_then(|n| n.rows.iter().find(|r| r.peer == peer))
                .map(|r| &r.toggle),
            NodeId::Join(group) => self
                .chat
                .as_ref()
                .and_then(|chat| {
                    chat.invite
                        .iter()
                        .chain(
                            chat.groups
                                .iter()
                                .flat_map(|g| &g.bubbles)
                                .filter_map(|b| b.invite.as_ref()),
                        )
                        .find(|i| i.group == group)
                })
                .and_then(|i| i.join.as_ref()),
            NodeId::Member(member) => self
                .members
                .as_ref()
                .and_then(|m| m.rows.iter().find(|r| r.address == member))
                .and_then(|r| r.remove.as_ref()),
            NodeId::SheetRow(row) => self.sheet.as_ref().and_then(|sheet| {
                sheet
                    .actions
                    .iter()
                    .find(|a| sheet_row(&a.intent) == Some(row))
            }),
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
        // The own name (1452 rule 4): the names' word for us, else the
        // name the profile has from the mailbox; never the own hash.
        Some(own) => Avatar::of(
            chat::initials(super::profile::own_name(model), &own),
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
        lead: Lead::Me {
            avatar,
            link: None,
            open: Some(Action::of(Intent::Profile)),
        },
        title: Text::key(StrId::TabChats),
        actions: Vec::from([
            Action::of(Intent::Search),
            Action::of(Intent::NewConversation),
        ]),
        a11y: A11y::of(NodeId::TopBar, Role::Bar, Text::key(StrId::TabChats)),
    }
}

/// The tab bar: Chats with the count of chats holding unread messages,
/// the map, and Explore (1578), always: three buttons and no fourth (Lew,
/// 2026-11-08). The browser and a hub's rooms are opened from Explore, so
/// Explore is the active tab while either is in front, and its badge is
/// the rooms' unread lines.
fn tabs<B>(model: &UiModel<B>) -> Tabs {
    let unread = model
        .listed()
        .filter(|&c| model.conversations[c].unread_rows() > 0)
        .count();
    let unread = u32::try_from(unread).unwrap_or(u32::MAX);
    let exploring = model.explore.open || model.browse.showing() || model.rooms.showing();
    let active = match model.map {
        _ if exploring => TabId::Explore,
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
    let rooms_unread: u32 = model.rooms.rooms.iter().map(|r| r.unread).sum();
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
            tab(
                TabId::Explore,
                StrId::TabExplore,
                (rooms_unread > 0).then_some(rooms_unread),
                Text::key(StrId::TabExplore),
            ),
        ]),
        active,
        a11y: A11y::of(NodeId::Tabs, Role::TabBar, Text::key(StrId::TabChats)),
    }
}

/// The list as the model orders it, its window built afresh: the cold
/// path, for a model changed outside `update` ([`UiModel::built_list`]
/// keeps the fold's). The window, never the whole order (1595): the M9's
/// fold hides the selection behind the list and gives it back outside
/// `update` (`loquax_m9::home`), so every draw of a pull came here, and
/// this built all 302 rows of the board's list where it shows seven
/// (window 20: 5,700 allocator calls and 282 ms a draw). The order is
/// the whole list's, its keys only; the rows are the screen's.
fn list<B>(model: &UiModel<B>) -> List {
    let order = model.order();
    let total = order.len();
    let (first, len, top) = model.list_window(&order);
    let window = order[first..first + len].to_vec();
    List {
        id: NodeId::List,
        rows: rows(model, window),
        first,
        total,
        top,
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
    // The pin says the position in the list (1224), the ring does not. A
    // group's avatar is how many are in it (1307).
    let group = super::group_scene::badge(model, peer);
    let initials = match &group {
        Some(badge) => super::group_scene::initials(model, peer, badge),
        None => row.initials.clone(),
    };
    let avatar = Avatar::of(initials, peer, row.icon.as_ref(), None);
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
    let same_name = model.names.same_name(peer);
    let spoken = match (&draft, row.last.is_empty()) {
        (Some(draft), _) => Text::Join(Vec::from([Text::Raw(row.name.clone()), draft.clone()])),
        (None, true) => Text::Raw(row.name.clone()),
        (None, false) => Text::Raw(format!("{}, {}", row.name, row.last)),
    };
    let spoken = match &same_name {
        Some(sentence) => Text::Join(Vec::from([spoken, sentence.clone()])),
        None => spoken,
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
        sharing: model.share(peer).is_some(),
        group,
        same_name,
        row,
    }
}

/// How long a share has left, in the catalogue's words, rounded up: `42
/// min`, `8 h`; none for one that runs until stopped.
pub fn share_left(share: &super::share::Share, now: i64) -> Option<Text> {
    let left = share.left_s(now)?;
    Some(match left {
        ..3600 => Text::Key(
            StrId::MinutesAgo,
            Vec::from([Arg::Int(((left + 59) / 60).max(1))]),
        ),
        _ => Text::Key(StrId::HoursAgo, Vec::from([Arg::Int((left + 3599) / 3600)])),
    })
}

/// What a running share says: "Sharing your position, 42 min left", or
/// "… until you stop".
pub fn share_words(share: &super::share::Share, now: i64) -> Text {
    match share_left(share, now) {
        Some(left) => Text::Key(StrId::SharingLeft, Vec::from([Arg::Text(Box::new(left))])),
        None => Text::key(StrId::SharingUntilStop),
    }
}

/// The chip of `peer`'s chat while our share runs there (1273).
pub fn share_chip<B>(model: &UiModel<B>, peer: &[u8; 16]) -> Option<ShareChip> {
    let share = model.share(peer)?;
    let now = model.now.map_or(share.started_at, |now| now.unix_secs);
    let words = match model.bar.by_hand {
        // A shell without a receiver shares the position entered by hand
        // (1371): the chip says so (1457), as the position sheet does.
        true => Text::Join(Vec::from([
            share_words(share, now),
            Text::key(StrId::FixByHand),
        ])),
        false => share_words(share, now),
    };
    Some(ShareChip {
        id: NodeId::ShareChip,
        a11y: A11y::of(NodeId::ShareChip, Role::Chip, words.clone()),
        words,
        stop: Action::of(Intent::StopShare { peer: *peer }),
    })
}

/// Settings, Privacy, "Position sharing" (1273): every running share, by
/// the peer's name, each with its Stop.
pub fn share_rows<B>(model: &UiModel<B>) -> Vec<ShareRow> {
    model
        .shares
        .running
        .iter()
        .map(|(peer, share)| {
            let now = model.now.map_or(share.started_at, |now| now.unix_secs);
            let name = model.names.label(peer, model.lang);
            let words = share_words(share, now);
            let id = NodeId::ShareRow(*peer);
            ShareRow {
                id,
                peer: *peer,
                a11y: A11y::of(
                    id,
                    Role::Row,
                    Text::Join(Vec::from([Text::Raw(name.clone()), words.clone()])),
                ),
                name,
                words,
                stop: Action::of(Intent::StopShare { peer: *peer }),
            }
        })
        .collect()
}

/// The peer's name in a chat's bar title: the title itself, but for a
/// relay's chat (1482) without the words "group via relay", for the
/// compose's hint and a quote's writer.
pub fn peer_title(bar: &TopBar) -> Text {
    name_of(&bar.title)
}

/// [`peer_title`] of a title: a shared name's sentence (1452) and a relay's
/// words left off.
fn name_of(title: &Text) -> Text {
    match title {
        Text::Key(StrId::RelayedGroup, args) => match args.first() {
            Some(Arg::Str(name)) => Text::Raw(name.clone()),
            _ => title.clone(),
        },
        Text::Join(parts) => parts.first().map_or_else(|| title.clone(), name_of),
        title => title.clone(),
    }
}

/// The open chat of conversation `c`: its bar, its bubbles grouped, its
/// compose. An index past the conversations gives an empty chat.
pub fn chat<B>(model: &UiModel<B>, c: usize) -> Chat {
    let to = model.conversations.get(c).map_or(0, |c| c.messages.len());
    chat_in(model, c, 0, to)
}

/// [`chat`] over the window `from..to` of its messages alone (1490):
/// what the fold builds for the viewport.
pub(crate) fn chat_in<B>(model: &UiModel<B>, c: usize, from: usize, to: usize) -> Chat {
    let peer = model.conversations.get(c).map_or([0; 16], |c| c.peer);
    let name = model.names.label(&peer, model.lang);
    let liveness = model.liveness(&peer);
    let badge = super::group_scene::badge(model, &peer);
    let avatar = Avatar::of(
        match &badge {
            Some(badge) => super::group_scene::initials(model, &peer, badge),
            None => chat::initials(model.names.shown(&peer), &peer),
        },
        &peer,
        model
            .conversations
            .get(c)
            .and_then(super::Conversation::appearance)
            .as_ref(),
        Ring::of(liveness.state),
    );
    // A group's bar opens its member sheet (1307), where the map would be.
    let bar_actions = match &badge {
        Some(_) => Vec::from([Action::of(Intent::Group(super::groups::Verb::Members {
            group: Target::Peer(peer),
        }))]),
        None => Vec::from([Action::of(Intent::OpenMapAt {
            peer: Target::Peer(peer),
        })]),
    };
    // A relay's chat says it is a group via a relay (1482).
    let title = match badge.is_none()
        && model
            .conversations
            .get(c)
            .is_some_and(super::relayed::is_relayed)
    {
        true => Text::Key(StrId::RelayedGroup, Vec::from([Arg::Str(name.clone())])),
        false => Text::Raw(name.clone()),
    };
    // Two peers of one name (1452): the header says so after the name.
    let same_name = model.names.same_name(&peer);
    let title = match &same_name {
        Some(sentence) => Text::Join(Vec::from([title, sentence.clone()])),
        None => title,
    };
    let bar = TopBar {
        id: NodeId::ChatBar,
        lead: Lead::Back {
            back: Action::of(Intent::CloseChat),
            avatar,
        },
        title: title.clone(),
        // The list's pin and the chat's bar open the map alike (1224).
        actions: bar_actions,
        a11y: A11y::of(NodeId::ChatBar, Role::Bar, title),
    };
    let bubbles = chat::bubbles_in(model, c, from, to);
    let places = chat::placements(&bubbles);
    let now = model.now;
    let in_group = model.groups.contains_key(&peer);
    let mut groups: Vec<Group> = Vec::new();
    for (bubble, place) in bubbles.into_iter().zip(places) {
        let playing = model.playing
            == Some(super::voice::Playing {
                peer,
                key: bubble.key,
            });
        let track = model.voice_track(peer, bubble.key);
        let invite = bubble
            .invite
            .and_then(|group| super::group_scene::invite(model, &group, bubble.ours));
        let mut node = bubble_node(peer, bubble, place, now, playing, track, in_group);
        node.invite = invite;
        match groups.last_mut() {
            Some(group) if place.joins_before => group.bubbles.push(node),
            _ => {
                let id = NodeId::Group(node.bubble.key);
                let ours = node.bubble.ours;
                let writer = node.bubble.writer.as_ref().map(|w| w.1.clone());
                let named = !ours && writer.is_some();
                let sender = match (ours, writer) {
                    (true, _) => Text::key(StrId::QuoteYou),
                    (false, Some(writer)) => Text::Raw(writer),
                    (false, None) => Text::Raw(name.clone()),
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
                    named,
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
        share: share_chip(model, &peer),
        groups,
        invite: badge
            .filter(|b| b.standing == super::groups::Standing::Invited)
            .and_then(|_| super::group_scene::invite(model, &peer, false)),
        compose: compose(model, c),
        ended: super::group_scene::ended(model, &peer),
        same_name,
        a11y: A11y::of(NodeId::Chat, Role::Chat, Text::Raw(name)),
    }
}

/// Conversation `c`'s compose bar, its draft in the field (1219).
pub fn compose<B>(model: &UiModel<B>, c: usize) -> Compose {
    let conversation = model.conversations.get(c);
    let peer = conversation.map_or([0; 16], |c| c.peer);
    let draft = conversation.map(|c| &c.draft);
    let round = model.round(peer);
    let staged = model.bar.staged.contains_key(&peer);
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
        round,
        buttons: compose::buttons(model.bar.abilities, round)
            .into_iter()
            .map(|button| compose_button(peer, button))
            .collect(),
        staged: model.bar.staged.get(&peer).map(|staged| {
            let words = Text::Key(
                StrId::Staged,
                Vec::from([Arg::Str(
                    super::photos::size_words(staged.data.len() as u64),
                )]),
            );
            StagedNode {
                id: NodeId::Staged,
                format: staged.format.clone(),
                bytes: staged.data.len(),
                a11y: A11y::of(NodeId::Staged, Role::Button, words.clone()),
                words,
                remove: Action::of(Intent::Unstage { peer }),
            }
        }),
        recording: recording_bar(model, peer),
        a11y: A11y::of(
            NodeId::Compose,
            Role::TextField,
            Text::key(match staged {
                true => StrId::CaptionHint,
                false => StrId::ComposeHint,
            }),
        ),
    }
}

/// One button of `peer`'s compose bar, its action and its words.
fn compose_button(peer: [u8; 16], button: Button) -> ComposeButton {
    let (id, intent) = match button {
        Button::Emoji => (NodeId::Emoji, Intent::Emoji),
        Button::Locate => (NodeId::Locate, Intent::Locate { peer }),
        Button::Camera => (NodeId::Camera, Intent::Camera { peer }),
        Button::Microphone => (
            NodeId::Record,
            Intent::SendVoice {
                peer,
                pcm: Vec::new(),
                rate: 0,
            },
        ),
        Button::Plus => (NodeId::Plus, Intent::Attach { peer }),
        Button::Send => (
            NodeId::Send,
            Intent::Send {
                to: Target::Peer(peer),
                body: None,
            },
        ),
    };
    let action = Action::of(intent);
    ComposeButton {
        id,
        button,
        a11y: A11y::of(id, Role::Button, Text::key(action.label_key)),
        action,
    }
}

/// The row of a sheet `intent` is, if it is one.
pub fn sheet_row(intent: &Intent) -> Option<SheetRow> {
    match intent {
        Intent::SendPhoto { .. } => Some(SheetRow::Gallery),
        Intent::SendFile { .. } => Some(SheetRow::File),
        Intent::Locate { .. } => Some(SheetRow::Position),
        Intent::ShareFix { .. } => Some(SheetRow::ShareNow),
        Intent::StartShare { span, .. } => Some(SheetRow::ShareFor(*span)),
        Intent::LocationOn => Some(SheetRow::LocationOn),
        Intent::EnterPosition { .. } => Some(SheetRow::EnterPosition),
        Intent::FixAtMapCentre => Some(SheetRow::MapCentre),
        _ => None,
    }
}

/// The sheet the compose bar of `peer`'s chat opened (1270): the plus's
/// rows, or the position sheet's one row and what it shows above it.
pub fn bar_sheet<B>(model: &UiModel<B>, peer: [u8; 16]) -> (Option<Sheet>, Option<Whereabouts>) {
    let Some(open) = model.bar.sheet.filter(|s| s.peer() == peer) else {
        return (None, None);
    };
    let (title, rows, whereabouts) = match open {
        compose::Sheet::Attach { .. } => (
            Text::key(StrId::Attach),
            compose::attach_rows(model.bar.abilities),
            None,
        ),
        compose::Sheet::Position { .. } => {
            let fix = model.bar.own_fix.as_ref();
            let words = match (fix, model.now) {
                (None, _) => Text::key(StrId::NoFix),
                (Some(_), _) if model.bar.by_hand => Text::key(StrId::FixByHand),
                (Some(fix), Some(now)) if now.unix_secs - fix.last_update >= 60 => Text::Key(
                    StrId::FixAge,
                    Vec::from([Arg::Text(Box::new(ago(now.unix_secs - fix.last_update)))]),
                ),
                (Some(_), _) => Text::key(StrId::FixNow),
            };
            let whereabouts = Whereabouts {
                fix: fix.map(|f| (f.latitude_e6, f.longitude_e6)),
                words,
            };
            // Whom the position goes to (1448, walk finding 16).
            (
                Text::Key(
                    StrId::SendPositionTo,
                    Vec::from([Arg::Str(model.names.label(&peer, model.lang))]),
                ),
                compose::position_rows(fix, model.bar.abilities, model.map_centre().is_some()),
                Some(whereabouts),
            )
        }
    };
    let actions = rows
        .into_iter()
        .map(|row| {
            Action::of(match row {
                SheetRow::Gallery => Intent::SendPhoto {
                    peer,
                    data: Vec::new(),
                },
                SheetRow::File => Intent::SendFile {
                    peer,
                    name: String::new(),
                    data: Vec::new(),
                    size: 0,
                },
                SheetRow::Position => Intent::Locate { peer },
                SheetRow::ShareNow => Intent::ShareFix { peer },
                SheetRow::ShareFor(span) => Intent::StartShare { peer, span },
                SheetRow::LocationOn => Intent::LocationOn,
                SheetRow::EnterPosition => Intent::EnterPosition { text: None },
                SheetRow::MapCentre => Intent::FixAtMapCentre,
            })
        })
        .collect();
    let sheet = Sheet {
        id: NodeId::Sheet,
        title: title.clone(),
        rows: Vec::new(),
        actions,
        a11y: A11y::of(NodeId::Sheet, Role::Sheet, title),
    };
    (Some(sheet), whereabouts)
}

fn bubble_node(
    peer: [u8; 16],
    bubble: Bubble,
    place: Placement,
    now: Option<WallTime>,
    playing: bool,
    track: Option<&LevelTrack>,
    in_group: bool,
) -> BubbleNode {
    let id = NodeId::Bubble(bubble.key);
    let now_secs = now.map(|now| now.unix_secs);
    let live = bubble.liveness.map(|l| row_live(&l, now_secs));
    let position = bubble.fix.as_ref().map(|fix| {
        let alive = bubble
            .liveness
            .is_some_and(|l| l.state == liveness::State::Live);
        position_words(fix, alive, now)
    });
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
            Vec::from([Arg::Str(super::photos::size_words(photo.bytes as u64))]),
        ),
        open: Action::of(Intent::ViewPhoto {
            peer,
            key: bubble.key,
        }),
        file: photo.file,
    });
    let files: Vec<FileNode> = bubble
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| FileNode {
            id: NodeId::File(bubble.key, index),
            name: super::files::safe_name(&file.name),
            bytes: file.bytes,
            size: super::photos::size_words(file.bytes as u64),
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
        // The recording's own track or the decoded sound's where the
        // model has one, the stream's packet sizes otherwise; the same
        // resampler either way (1276).
        levels: track.map_or_else(|| voice.levels.clone(), |t| t.bars(BARS, Fold::Peak)),
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
    let spoken = match (
        &live,
        &photo,
        files.first(),
        &voice,
        bubble.body.trim().is_empty(),
    ) {
        (Some(live), _, _, _, true) => live.clone(),
        // A picture sent as a file is said as its file, by its name (1662).
        (_, Some(photo), _, _, true) if photo.file.is_none() => photo.words.clone(),
        (_, _, Some(file), _, true) => file.words.clone(),
        (_, _, _, Some(voice), true) => voice.words.clone(),
        _ => Text::Raw(bubble.body.clone()),
    };
    let mark = bubble.mark.map(|mark| MarkNode {
        mark,
        // A group's two ticks are every member's device (1466): the
        // devices, not one recipient's.
        words: match (in_group, mark) {
            (true, chat::Mark::Delivered) => Text::key(StrId::StateGroupDelivered),
            _ => mark_words(mark, bubble.read_at, bubble.witness, now),
        },
        activate: (mark == chat::Mark::Failed).then(|| {
            Action::of(Intent::SendAgain {
                peer,
                key: bubble.key,
            })
        }),
    });
    let progress = bubble.transfer.as_ref().map(|transfer| {
        Box::new(ProgressNode {
            id: NodeId::Progress(bubble.key),
            percent: transfer.percent(),
            words: transfer.words(),
        })
    });
    let text = (!bubble.body.trim().is_empty()).then(|| bubble.body.clone());
    BubbleNode {
        id,
        a11y: A11y::of(id, Role::Bubble, spoken),
        text,
        bubble,
        place,
        live,
        position,
        mark,
        photo,
        files,
        voice,
        invite: None,
        progress,
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

/// A shared position's sentence in a chat (1449 item 5, walk 1 finding
/// 11), the one rule of every target: `Live position` while the reports
/// still come, else when the fix was taken, `Position from yesterday
/// 21:46`. The coordinates and the accuracy are the Info's, never the
/// bubble's.
pub fn position_words(fix: &crate::telemetry::Fix, live: bool, now: Option<WallTime>) -> Text {
    if live {
        return Text::key(StrId::PositionLive);
    }
    // A claim ahead of our clock is its receive time, as everywhere.
    let at = WallTime {
        unix_secs: fix.order_time(),
        offset_secs: now.map_or(0, |now| now.offset_secs),
    };
    let clock = Arg::Str(at.hh_mm());
    let c = at.civil();
    match now.map(|now| now.day() - at.day()) {
        Some(..=0) => Text::Key(StrId::PositionToday, Vec::from([clock])),
        Some(1) => Text::Key(StrId::PositionYesterday, Vec::from([clock])),
        Some(2..=6) => Text::Key(
            StrId::PositionWeekday,
            Vec::from([
                Arg::Text(Box::new(Text::key(WEEKDAYS[c.weekday as usize % 7]))),
                clock,
            ]),
        ),
        Some(_) => Text::Key(
            StrId::PositionDate,
            Vec::from([
                Arg::Text(Box::new(list_time(&at, &now.unwrap_or(at)))),
                clock,
            ]),
        ),
        // No clock said yet: the whole date, as the day line says it.
        None => Text::Key(
            StrId::PositionDate,
            Vec::from([Arg::Text(Box::new(day_words(&at, None))), clock]),
        ),
    }
}

/// The conversation list's time (1449 item 4, walk 1 finding 10), the one
/// rule of every target's list: the clock today (`17:37`), `Yesterday`,
/// the weekday short within the six days before (`Do.`), else the date
/// (`22.10.`, `22 Oct`), with the year when it is not this one. Never an
/// age: "17 Min." and "7h" beside "Do." were three formats in one column.
pub fn list_time(at: &WallTime, now: &WallTime) -> Text {
    let c = at.civil();
    match now.day() - at.day() {
        ..=0 => return Text::Raw(at.hh_mm()),
        1 => return Text::key(StrId::Yesterday),
        2..=6 => return Text::key(WEEKDAYS_SHORT[c.weekday as usize % 7]),
        _ => {}
    }
    let month = Text::key(MONTHS_SHORT[(c.month as usize).saturating_sub(1) % 12]);
    let mut args = Vec::from([Arg::Int(i64::from(c.day)), Arg::Text(Box::new(month))]);
    match c.year == now.civil().year {
        true => Text::Key(StrId::ListDate, args),
        false => {
            args.push(Arg::Int(c.year));
            Text::Key(StrId::ListDateYear, args)
        }
    }
}

const MONTHS_SHORT: [StrId; 12] = [
    StrId::JanuaryShort,
    StrId::FebruaryShort,
    StrId::MarchShort,
    StrId::AprilShort,
    StrId::MayShort,
    StrId::JuneShort,
    StrId::JulyShort,
    StrId::AugustShort,
    StrId::SeptemberShort,
    StrId::OctoberShort,
    StrId::NovemberShort,
    StrId::DecemberShort,
];

/// The weekday of `at`, in full: "Monday".
pub fn weekday(at: &WallTime) -> Text {
    Text::key(WEEKDAYS[at.civil().weekday as usize % 7])
}

const WEEKDAYS_SHORT: [StrId; 7] = [
    StrId::MondayShort,
    StrId::TuesdayShort,
    StrId::WednesdayShort,
    StrId::ThursdayShort,
    StrId::FridayShort,
    StrId::SaturdayShort,
    StrId::SundayShort,
];

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
        let mut value = json!({
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
            "sharing": self.sharing,
            "selected": self.item.selected,
            "activate": self.item.activate.to_json(),
            "menu": actions(&self.item.menu),
            "a11y": self.item.a11y.to_json(),
        });
        if let (Some(group), Value::Object(map)) = (&self.group, &mut value) {
            map.insert("group".into(), group.to_json());
        }
        if let (Some(same), Value::Object(map)) = (&self.same_name, &mut value) {
            map.insert("same_name".into(), same.to_json());
        }
        value
    }
}

impl BubbleNode {
    pub fn to_json(&self) -> Value {
        let b = &self.bubble;
        let mut value = json!({
            "id": self.id.resource(),
            "msg_id": opt(b.id, |id| hex(&id)),
            "ours": b.ours,
            "body": b.body,
            "text": self.text,
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
            "photo": opt(self.photo.as_ref(), |p| {
                let mut photo = json!({
                    "id": p.id.resource(),
                    "format": p.format,
                    "bytes": p.bytes,
                    "words": p.words.to_json(),
                    "open": p.open.to_json(),
                });
                // A picture sent as a file only (1662): the goldens of
                // a photo stay as they were.
                if let (Some(file), Value::Object(map)) = (p.file, &mut photo) {
                    map.insert("file".into(), json!(file));
                }
                photo
            }),
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
        });
        // A group's keys only where there is a group (1307): the goldens
        // of a plain chat stay as they were.
        if let Value::Object(map) = &mut value {
            // Bytes under way only while they go (1662), as a group's.
            if let Some(p) = &self.progress {
                map.insert(
                    "progress".into(),
                    json!({
                        "id": p.id.resource(),
                        "percent": p.percent,
                        "words": p.words.to_json(),
                    }),
                );
            }
            if let Some(invite) = &self.invite {
                map.insert("invite".into(), invite.to_json());
            }
            if let Some((writer, name)) = &b.writer {
                map.insert(
                    "writer".into(),
                    json!({ "address": hex(writer), "name": name }),
                );
            }
            if b.note {
                map.insert("note".into(), Value::Bool(true));
            }
            // A line that arrived late (1307 pass four), only where one is.
            if b.late {
                map.insert("late".into(), Text::key(StrId::GroupArrivedLate).to_json());
            }
        }
        value
    }
}

impl Chat {
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "id": self.id.resource(),
            "peer": hex(&self.peer),
            "bar": self.bar.to_json(),
            "share": opt(self.share.as_ref(), |chip| json!({
                "id": chip.id.resource(),
                "words": chip.words.to_json(),
                "stop": chip.stop.to_json(),
                "a11y": chip.a11y.to_json(),
            })),
            "groups": self.groups.iter().map(|g| {
                let mut group = json!({
                    "id": g.id.resource(),
                    "ours": g.ours,
                    "sender": g.sender.to_json(),
                    "day": opt(g.day.as_ref(), |d| json!({ "at": d.at.unix_secs, "words": d.words.to_json() })),
                    "bubbles": g.bubbles.iter().map(BubbleNode::to_json).collect::<Vec<_>>(),
                    "a11y": g.a11y.to_json(),
                });
                if let (true, Value::Object(map)) = (g.named, &mut group) {
                    map.insert("named".into(), json!(true));
                }
                group
            }).collect::<Vec<_>>(),
            "compose": {
                "id": self.compose.id.resource(),
                "hint": self.compose.hint.to_json(),
                "text": self.compose.text,
                "reply": opt(self.compose.reply, |id| hex(&id)),
                "send": self.compose.send.to_json(),
                "record": self.compose.record.to_json(),
                "round": self.compose.round.word(),
                "buttons": self.compose.buttons.iter().map(|b| json!({
                    "id": b.id.resource(),
                    "inside": b.button.inside(),
                    "action": b.action.to_json(),
                    "a11y": b.a11y.to_json(),
                })).collect::<Vec<_>>(),
                "staged": opt(self.compose.staged.as_ref(), |st| json!({
                    "id": st.id.resource(),
                    "format": st.format,
                    "bytes": st.bytes,
                    "words": st.words.to_json(),
                    "remove": st.remove.to_json(),
                })),
                "recording": opt(self.compose.recording.as_ref(), RecordingBar::to_json),
                "a11y": self.compose.a11y.to_json(),
            },
            "a11y": self.a11y.to_json(),
        });
        if let (Some(invite), Value::Object(map)) = (&self.invite, &mut value) {
            map.insert("invite".into(), invite.to_json());
        }
        if let (Some(ended), Value::Object(map)) = (&self.ended, &mut value) {
            map.insert("ended".into(), ended.to_json());
        }
        value
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
            ScreenId::Browse => json!("browse"),
            ScreenId::Rooms => json!("rooms"),
            ScreenId::Explore => json!("explore"),
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
            opt(self.sheet.as_ref(), |s| {
                json!({
                    "id": s.id.resource(),
                    "title": s.title.to_json(),
                    "rows": s.actions.iter().map(|a| json!({
                        "id": sheet_row(&a.intent).map(|r| NodeId::SheetRow(r).resource()),
                        "action": a.to_json(),
                    })).collect::<Vec<_>>(),
                })
            }),
        );
        map.insert(
            "whereabouts".into(),
            opt(self.whereabouts.as_ref(), |w| {
                json!({ "fix": w.fix.map(|(lat, lon)| [lat, lon]), "words": w.words.to_json() })
            }),
        );
        map.insert(
            "card".into(),
            opt(self.card.as_ref(), |c| json!(c.id.resource())),
        );
        map.insert(
            "chips".into(),
            Value::Array(self.chips.iter().map(|c| json!(c.id.resource())).collect()),
        );
        if let Some(new_group) = &self.new_group {
            map.insert("new_group".into(), new_group.to_json());
        }
        if let Some(members) = &self.members {
            map.insert("members".into(), members.to_json());
        }
        if let Some(browse) = &self.browse {
            map.insert("browse".into(), browse.to_json());
        }
        if let Some(rooms) = &self.rooms {
            map.insert("rooms".into(), rooms.to_json());
        }
        if let Some(explore) = &self.explore {
            map.insert("explore".into(), explore.to_json());
        }
        if let Some(profile) = &self.profile {
            map.insert("profile".into(), profile.to_json());
        }
        Value::Object(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1506: a bubble read at no known time (an import's) is "Read", never
    /// the epoch's "Read at 00:00"; a known time is said.
    #[test]
    fn a_read_without_its_time_says_read_alone() {
        let untimed = mark_words(chat::Mark::Read, None, None, None);
        assert_eq!(untimed, Text::key(StrId::StateReadUntimed));
        assert_eq!(untimed.resolve(super::super::strings::Lang::En), "Read");
        assert_eq!(
            mark_words(chat::Mark::Read, Some(0), None, None),
            Text::key(StrId::StateReadUntimed)
        );
        let timed = mark_words(chat::Mark::Read, Some(3_600 * 10 + 60 * 32), None, None);
        assert_eq!(
            timed.resolve(super::super::strings::Lang::En),
            "Read at 10:32"
        );
    }
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

    /// 1270: the compose node carries the button set as data, the plus
    /// turns into Send on typing and with a staged photo, the plus and
    /// the position button open their sheets as `Sheet` nodes with their
    /// rows, the position sheet shows the own fix and its age or says
    /// there is none, and every node's words are the catalogue's.
    #[test]
    fn the_compose_bar_has_its_buttons_and_its_sheets() {
        let mut model = model_with(&[(false, 1_793_350_800)]);
        let peer = [0x42; 16];
        model.selected = 0;
        model.bar.abilities = compose::Abilities::phone(true);
        let ids = |model: &UiModel<Line>| -> Vec<String> {
            let scene = model.scene(LayoutClass::Compact);
            let compose = &scene.chat.as_ref().expect("a chat").compose;
            compose.buttons.iter().map(|b| b.id.resource()).collect()
        };
        assert_eq!(ids(&model), ["emoji", "locate", "camera", "record", "plus"]);
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(
            scene.action(NodeId::Plus).map(|a| a.intent.clone()),
            Some(Intent::Attach { peer })
        );
        assert_eq!(
            scene.action(NodeId::Camera).map(|a| &a.intent),
            Some(&Intent::Camera { peer })
        );
        let compose = &scene.chat.as_ref().expect("a chat").compose;
        let words: Vec<_> = compose
            .buttons
            .iter()
            .map(|b| b.a11y.label.resolve(Lang::De))
            .collect();
        assert_eq!(
            words,
            [
                "Emoji",
                "Meine Position",
                "Foto aufnehmen",
                "Halten für eine Sprachnachricht",
                "Anhängen"
            ]
        );
        assert!(scene.sheet.is_none());

        // Typing: Send, and the microphone hides.
        crate::ui::update(
            &mut model,
            UiEvent::Draft {
                peer,
                text: "hi".into(),
                reply: None,
            },
        );
        assert_eq!(ids(&model), ["emoji", "locate", "camera", "send"]);
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(
            scene.action(NodeId::Send).map(|a| a.intent.clone()),
            Some(Intent::Send {
                to: Target::Peer(peer),
                body: None
            })
        );
        assert!(
            scene.action(NodeId::Record).is_some(),
            "held, it still is the record"
        );

        // A staged photo with an empty field: Send, its node, the caption hint.
        crate::ui::update(
            &mut model,
            UiEvent::Draft {
                peer,
                text: String::new(),
                reply: None,
            },
        );
        crate::ui::update(
            &mut model,
            UiEvent::StagePhoto {
                peer,
                format: "jpg".into(),
                data: Vec::from([7; 1_500]),
            },
        );
        assert_eq!(ids(&model), ["emoji", "locate", "camera", "send"]);
        let scene = model.scene(LayoutClass::Compact);
        let compose = &scene.chat.as_ref().expect("a chat").compose;
        let staged = compose.staged.as_ref().expect("staged");
        assert_eq!(staged.words.resolve(Lang::En), "Photo to send, 2 KB");
        assert_eq!(staged.remove.intent, Intent::Unstage { peer });
        assert_eq!(compose.a11y.label.resolve(Lang::En), "Add a caption");
        crate::ui::update(&mut model, UiEvent::Unstage { peer });

        // The plus's sheet: Gallery, File, Position.
        crate::ui::update(
            &mut model,
            UiEvent::OpenSheet(compose::Sheet::Attach { peer }),
        );
        let scene = model.scene(LayoutClass::Compact);
        let sheet = scene.sheet.as_ref().expect("the attachment sheet");
        assert_eq!(sheet.title.resolve(Lang::En), "Attach");
        let rows: Vec<_> = sheet
            .actions
            .iter()
            .map(|a| {
                (
                    sheet_row(&a.intent).map(|r| NodeId::SheetRow(r).resource()),
                    Text::key(a.label_key).resolve(Lang::En),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (Some("sheet_gallery".to_string()), "Gallery".to_string()),
                (Some("sheet_file".to_string()), "File".to_string()),
                (
                    Some("sheet_position".to_string()),
                    "My position".to_string()
                ),
            ]
        );
        assert!(scene.whereabouts.is_none());
        assert_eq!(
            scene
                .action(NodeId::SheetRow(SheetRow::Position))
                .map(|a| a.intent.clone()),
            Some(Intent::Locate { peer })
        );

        // The position sheet: no fix, then one five minutes old.
        crate::ui::update(
            &mut model,
            UiEvent::OpenSheet(compose::Sheet::Position { peer }),
        );
        let scene = model.scene(LayoutClass::Compact);
        let there = scene.whereabouts.as_ref().expect("whereabouts");
        assert_eq!(there.fix, None);
        assert_eq!(
            there.words.resolve(Lang::En),
            "Loquax does not know where you are yet."
        );
        assert!(scene
            .action(NodeId::SheetRow(SheetRow::LocationOn))
            .is_some());
        let now = 1_793_351_000;
        crate::ui::update(
            &mut model,
            UiEvent::Clock(WallTime {
                unix_secs: now,
                offset_secs: 0,
            }),
        );
        crate::ui::update(
            &mut model,
            UiEvent::OwnFix(Some(leviculum_lxmf::telemetry::Location {
                latitude_e6: 43_773_100,
                longitude_e6: 11_256_000,
                altitude_e2: 0,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 500,
                last_update: now - 300,
            })),
        );
        let scene = model.scene(LayoutClass::Compact);
        let there = scene.whereabouts.as_ref().expect("whereabouts");
        assert_eq!(there.fix, Some((43_773_100, 11_256_000)));
        assert_eq!(there.words.resolve(Lang::En), "Found 5 min ago");
        assert_eq!(there.words.resolve(Lang::De), "Ermittelt vor 5 Min.");
        let sheet = scene.sheet.as_ref().expect("the position sheet");
        // Whom it goes to (1448, walk finding 16), by the name every
        // screen gives the peer, in the model's language: nameless here,
        // so "Unknown".
        assert_eq!(sheet.title.resolve(Lang::En), "Send position to Unknown");
        assert_eq!(sheet.title.resolve(Lang::De), "Position an Unknown senden");
        assert_eq!(
            scene
                .action(NodeId::SheetRow(SheetRow::ShareNow))
                .map(|a| a.intent.clone()),
            Some(Intent::ShareFix { peer })
        );
        let json = scene.to_json();
        assert_eq!(json["sheet"]["rows"][0]["id"], "sheet_share_now");
        assert_eq!(json["chat"]["compose"]["round"], "plus");

        // Another chat's sheet is not this one's.
        crate::ui::update(
            &mut model,
            UiEvent::OpenSheet(compose::Sheet::Attach { peer: [0x43; 16] }),
        );
        assert!(model.scene(LayoutClass::Compact).sheet.is_none());
    }

    /// A shell that can do nothing of it (the window today) shows the
    /// emoji and Send alone, and no plus with nothing in it.
    #[test]
    fn a_window_without_camera_or_picker_shows_emoji_and_send() {
        let mut model = model_with(&[(false, 1_793_350_800)]);
        model.selected = 0;
        let scene = model.scene(LayoutClass::Wide);
        let compose = &scene.chat.as_ref().expect("a chat").compose;
        let ids: Vec<_> = compose.buttons.iter().map(|b| b.id.resource()).collect();
        assert_eq!(ids, ["emoji", "send"]);
        assert_eq!(compose.round, Round::Send);
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

    /// 1449 item 5 (walk 1 finding 11): a shared position is one sentence,
    /// `Live position` while its reports come, else when its fix was taken,
    /// in both languages; never the coordinates.
    #[test]
    fn a_shared_position_is_one_sentence_never_coordinates() {
        let at = |unix_secs| WallTime {
            unix_secs,
            offset_secs: 3600,
        };
        // 2026-10-30 10:00 local, a Friday.
        let t = 1_793_350_800;
        let now = at(t);
        let fix = |taken: i64| crate::telemetry::Fix {
            peer: [1; 16],
            via: [1; 16],
            message: None,
            received_at: Some(t),
            location: leviculum_lxmf::telemetry::Location {
                latitude_e6: 53_105_960,
                longitude_e6: 9_159_300,
                altitude_e2: 0,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 10_200,
                last_update: taken,
            },
            telemetry: Vec::new(),
            source: crate::telemetry::Source::Telemetry,
        };
        let said = |taken: i64, live: bool, lang| {
            position_words(&fix(taken), live, Some(now)).resolve(lang)
        };
        assert_eq!(said(t - 60, true, Lang::De), "Live-Position");
        assert_eq!(said(t - 60, true, Lang::En), "Live position");
        assert_eq!(said(t - 600, false, Lang::De), "Position von heute 09:50");
        // Yesterday 21:46 local.
        let yesterday = t - (10 * 3600) - (2 * 3600 + 14 * 60);
        assert_eq!(
            said(yesterday, false, Lang::De),
            "Position von gestern 21:46"
        );
        assert_eq!(
            said(yesterday, false, Lang::En),
            "Position from yesterday 21:46"
        );
        assert_eq!(
            said(t - 3 * 86_400, false, Lang::De),
            "Position von Dienstag 10:00"
        );
        assert_eq!(
            said(t - 9 * 86_400, false, Lang::De),
            "Position vom 21.10. 10:00"
        );
        for lang in [Lang::De, Lang::En] {
            let words = said(t - 600, false, lang);
            assert!(!words.contains("53.") && !words.contains('±'), "{words}");
        }
        // A claim ahead of our clock is said at its receive time.
        assert_eq!(said(t + 7200, false, Lang::De), "Position von heute 10:00");
    }

    /// 1449 item 4 (walk 1 finding 10): one format in the list's time
    /// column on every target, in both languages; never an age.
    #[test]
    fn the_list_time_is_one_format_the_clock_yesterday_weekday_date() {
        let at = |unix_secs| WallTime {
            unix_secs,
            offset_secs: 3600,
        };
        // 2026-10-30 10:00 local, a Friday.
        let t = 1_793_350_800;
        let now = at(t);
        let both = |secs_before: i64| {
            let shown = list_time(&at(t - secs_before), &now);
            (shown.resolve(Lang::De), shown.resolve(Lang::En))
        };
        assert_eq!(
            both(17 * 60),
            ("09:43".into(), "09:43".into()),
            "not 17 Min."
        );
        assert_eq!(both(7 * 3600), ("03:00".into(), "03:00".into()), "not 7h");
        assert_eq!(both(86_400), ("Gestern".into(), "Yesterday".into()));
        assert_eq!(both(2 * 86_400), ("Mi.".into(), "Wed".into()));
        assert_eq!(both(6 * 86_400), ("Sa.".into(), "Sat".into()));
        assert_eq!(both(8 * 86_400), ("22.10.".into(), "22 Oct".into()));
        assert_eq!(
            both(365 * 86_400),
            ("30.10.2025".into(), "30 Oct 2025".into())
        );
        // A time ahead of the clock is today's.
        assert_eq!(list_time(&at(t + 60), &now).resolve(Lang::De), "10:01");
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

    /// The position sheet's five rows (1273), a share's chip with its
    /// Stop at the top of the chat, the ⌖ on the list's row, and
    /// Settings, Privacy's list; the stop takes all of them away.
    #[test]
    fn a_share_shows_its_chip_its_sign_and_its_privacy_row_until_stopped() {
        use super::super::share::Span;
        let mut model = model_with(&[(false, 1_793_350_800)]);
        let peer = [0x42; 16];
        model.selected = 0;
        model.bar.abilities = compose::Abilities::phone(true);
        let now = 1_793_351_000;
        crate::ui::update(
            &mut model,
            UiEvent::Clock(WallTime {
                unix_secs: now,
                offset_secs: 0,
            }),
        );
        crate::ui::update(
            &mut model,
            UiEvent::OwnFix(Some(leviculum_lxmf::telemetry::Location {
                latitude_e6: 43_773_100,
                longitude_e6: 11_256_000,
                altitude_e2: 0,
                speed_e2: 0,
                bearing_e2: 0,
                accuracy_e2: 500,
                last_update: now,
            })),
        );
        crate::ui::update(
            &mut model,
            UiEvent::OpenSheet(compose::Sheet::Position { peer }),
        );
        let scene = model.scene(LayoutClass::Compact);
        let sheet = scene.sheet.as_ref().expect("the position sheet");
        let rows: Vec<_> = sheet
            .actions
            .iter()
            .map(|a| {
                (
                    sheet_row(&a.intent).map(|r| NodeId::SheetRow(r).resource()),
                    Text::key(a.label_key).resolve(Lang::En),
                    Text::key(a.label_key).resolve(Lang::De),
                )
            })
            .collect();
        let row = |id: &str, en: &str, de: &str| (Some(id.to_string()), en.into(), de.into());
        assert_eq!(
            rows,
            [
                row("sheet_share_now", "Now, once", "Jetzt, einmal"),
                row("sheet_share_15_min", "For 15 minutes", "Für 15 Minuten"),
                row("sheet_share_1_h", "For 1 hour", "Für 1 Stunde"),
                row("sheet_share_8_h", "For 8 hours", "Für 8 Stunden"),
                row(
                    "sheet_share_until_stopped",
                    "Until I stop",
                    "Bis ich es beende"
                ),
            ]
        );
        assert!(scene.chat.as_ref().expect("chat").share.is_none());
        let start = scene
            .action(NodeId::SheetRow(SheetRow::ShareFor(Span::Hour1)))
            .map(|a| a.intent.clone());
        assert_eq!(
            start,
            Some(Intent::StartShare {
                peer,
                span: Span::Hour1
            })
        );
        crate::ui::update(
            &mut model,
            UiEvent::StartShare {
                peer,
                span: Span::Hour1,
            },
        );
        crate::ui::update(
            &mut model,
            UiEvent::Clock(WallTime {
                unix_secs: now + 18 * 60,
                offset_secs: 0,
            }),
        );
        let scene = model.scene(LayoutClass::Compact);
        assert!(scene.sheet.is_none(), "the row closed the sheet");
        let chip = scene
            .chat
            .as_ref()
            .and_then(|c| c.share.as_ref())
            .expect("the chip");
        assert_eq!(
            chip.words.resolve(Lang::En),
            "Sharing your position, 42 min left"
        );
        assert_eq!(
            chip.words.resolve(Lang::De),
            "Deine Position wird geteilt, noch 42 Min."
        );
        assert_eq!(Text::key(chip.stop.label_key).resolve(Lang::En), "Stop");
        assert_eq!(
            scene.action(NodeId::ShareStop).map(|a| a.intent.clone()),
            Some(Intent::StopShare { peer })
        );
        let wide = model.scene(LayoutClass::Wide);
        let listed = wide.list.as_ref().expect("list");
        assert!(listed
            .rows
            .iter()
            .all(|r| r.sharing == (r.row.peer == peer)));
        assert_eq!(wide.to_json()["list"]["rows"][0]["sharing"], true);
        assert_eq!(wide.to_json()["chat"]["share"]["id"], "share_chip");
        let privacy = share_rows(&model);
        assert_eq!(privacy.len(), 1);
        assert_eq!(privacy[0].id.resource(), format!("share_{}", to_hex(&peer)));
        assert_eq!(privacy[0].stop.intent, Intent::StopShare { peer });

        // Until stopped, and then stopped.
        crate::ui::update(
            &mut model,
            UiEvent::StartShare {
                peer,
                span: Span::UntilStopped,
            },
        );
        let scene = model.scene(LayoutClass::Compact);
        let chip = scene
            .chat
            .as_ref()
            .and_then(|c| c.share.as_ref())
            .expect("chip");
        assert_eq!(
            chip.words.resolve(Lang::En),
            "Sharing your position until you stop"
        );
        crate::ui::update(&mut model, UiEvent::StopShare { peer });
        let scene = model.scene(LayoutClass::Wide);
        assert!(scene.chat.as_ref().expect("chat").share.is_none());
        assert!(scene
            .list
            .as_ref()
            .expect("list")
            .rows
            .iter()
            .all(|r| !r.sharing));
        assert!(share_rows(&model).is_empty());
    }

    /// The time left, rounded up: under a minute is one, an hour and a
    /// bit are two hours.
    #[test]
    fn the_time_left_rounds_up() {
        use super::super::share::{Share, Span};
        let share = Share::start(Span::Hours8, 0);
        let left = |at| share_left(&share, at).map(|t| t.resolve(Lang::En));
        assert_eq!(left(0).as_deref(), Some("8 h"));
        assert_eq!(left(8 * 3600 - 3601).as_deref(), Some("2 h"));
        assert_eq!(left(8 * 3600 - 3600).as_deref(), Some("1 h"));
        assert_eq!(left(8 * 3600 - 3599).as_deref(), Some("60 min"));
        assert_eq!(left(8 * 3600 - 10).as_deref(), Some("1 min"));
        assert_eq!(left(9 * 3600).as_deref(), Some("1 min"));
        assert_eq!(share_left(&Share::start(Span::UntilStopped, 0), 5), None);
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
            DeliveryState::Arrived,
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
        model.conversations[0]
            .messages
            .for_each_indexed_mut(|m, message| message.delivery = Some(states[m]));
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
            // 1462: the proof is one tick and neutral, the device's own
            // word is two.
            (chat::Mark::Arrived, "Arrived", "Angekommen"),
            (
                chat::Mark::Delivered,
                "On the recipient's device",
                "Auf dem Gerät",
            ),
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
        model.conversations[0]
            .messages
            .get_mut(0)
            .expect("message")
            .delivery = Some(DeliveryState::Failed);
        let delivered = super::super::update(&mut model, UiEvent::SendAgain { peer, key: 2 });
        assert!(delivered.is_empty(), "a delivered one is not sent again");
        let effects = super::super::update(&mut model, UiEvent::SendAgain { peer, key: 1 });
        // The new send, and what the failed one saved goes with it (1662).
        assert!(
            matches!(
                effects.as_slice(),
                [
                    UiEffect::Send { to, body, .. },
                    UiEffect::ForgetSaved { peer: gone, key: Some(1) },
                ] if *to == peer && body == "m0" && *gone == peer
            ),
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

    /// A photo without words is one bubble (1268): the photo node, the
    /// mark, and no text run, so no renderer draws an empty row under the
    /// picture; with words the run is the body, and a body of blanks is
    /// none. A file or a voice message alone is the same.
    #[test]
    fn a_photo_without_words_is_one_bubble_with_media_a_mark_and_no_text() {
        use crate::ui::files::file_fields;
        use crate::ui::photos::photo_fields;
        use crate::ui::voice::{voice_fields, AM_OPUS_OGG};
        let t = 1_793_350_800;
        let mut model = model_with(&[(true, t), (true, t + 60), (true, t + 120), (true, t + 180)]);
        let messages = &mut model.conversations[0].messages;
        messages.get_mut(0).expect("message").body = String::new();
        messages.get_mut(0).expect("message").fields = photo_fields("jpg", alloc::vec![7; 312_400]);
        messages.get_mut(1).expect("message").body = " \n".into();
        messages.get_mut(1).expect("message").fields =
            file_fields("report.pdf", alloc::vec![7; 1_400]);
        messages.get_mut(2).expect("message").body = String::new();
        messages.get_mut(2).expect("message").fields =
            voice_fields(AM_OPUS_OGG, alloc::vec![7; 900]);
        messages.get_mut(3).expect("message").body = "the view".into();
        messages.get_mut(3).expect("message").fields = photo_fields("jpg", alloc::vec![7; 1_000]);
        model.selected = 0;
        let scene = model.scene(LayoutClass::Compact);
        let groups = &scene.chat.as_ref().expect("the chat").groups;
        let bubbles: Vec<&BubbleNode> = groups.iter().flat_map(|g| &g.bubbles).collect();
        assert_eq!(bubbles.len(), 4, "one bubble a message: {bubbles:#?}");

        let photo = scene.bubble(NodeId::Bubble(1)).expect("the photo's bubble");
        assert!(photo.photo.is_some(), "the photo is in the bubble");
        assert_eq!(photo.text, None, "no text run");
        assert!(photo.mark.is_some(), "the mark is in the bubble");
        assert_eq!(photo.to_json()["text"], Value::Null);
        let file = scene.bubble(NodeId::Bubble(2)).expect("the file's bubble");
        assert_eq!((file.files.len(), file.text.as_deref()), (1, None));
        assert_eq!(file.a11y.label, file.files[0].words, "said as the file");
        let voice = scene.bubble(NodeId::Bubble(3)).expect("the voice's bubble");
        assert!(voice.voice.is_some() && voice.text.is_none() && voice.mark.is_some());
        let worded = scene.bubble(NodeId::Bubble(4)).expect("photo and words");
        assert!(worded.photo.is_some());
        assert_eq!(worded.text.as_deref(), Some("the view"));
        assert_eq!(worded.to_json()["text"], "the view");
        // The last of the group closes it with its time and its mark.
        assert!(worded.place.meta && worded.mark.is_some());
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
        model.conversations[0]
            .messages
            .get_mut(1)
            .expect("message")
            .body = String::new();
        model.conversations[0]
            .messages
            .get_mut(1)
            .expect("message")
            .fields = photo_fields("jpg", alloc::vec![7; 312_400]);
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
        assert_eq!(
            chat::preview(&model.conversations[0].messages[1], Lang::En),
            "Photo"
        );
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
        model.conversations[0]
            .messages
            .get_mut(1)
            .expect("message")
            .body = String::new();
        model.conversations[0]
            .messages
            .get_mut(1)
            .expect("message")
            .fields = file_fields("report.pdf", alloc::vec![7; 312_400]);
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
            chat::preview(&model.conversations[0].messages[1], Lang::En),
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
        model.conversations[0]
            .messages
            .get_mut(1)
            .expect("message")
            .body = String::new();
        model.conversations[0]
            .messages
            .get_mut(1)
            .expect("message")
            .fields = voice_fields(AM_CODEC2_2400, alloc::vec![0; 600]);
        model.conversations[0]
            .messages
            .get_mut(2)
            .expect("message")
            .fields = voice_fields(AM_CODEC2_700C, alloc::vec![0; 200]);
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
            chat::preview(&model.conversations[0].messages[1], Lang::En),
            "Voice message, 0:02"
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

    /// The recording bar (1276): held, the bar with its timer, live
    /// waveform, cancel hint and open lock chip; locked, the bin, pause
    /// and Send; paused, the waveform stands and play and resume join;
    /// nothing heard, the line and the way to the permission; sent, the
    /// field is back and the bubble's waveform is the recording's track
    /// through the one resampler.
    #[test]
    fn a_recording_shows_as_recording_and_its_track_becomes_the_bubble() {
        use super::super::recording::{Effect, Input, LIVE_WINDOWS};
        use super::super::UiEffect;
        let t = 1_793_350_800;
        let mut model = model_with(&[(false, t)]);
        let peer = [0x42; 16];
        model.selected = 0;
        // Out of the way of the key the model gives the one it sends.
        model.conversations[0]
            .messages
            .get_mut(0)
            .expect("message")
            .activity = 1_000;
        let feed = |model: &mut UiModel<Line>, input| {
            super::super::update(model, UiEvent::Record { peer, input })
        };
        let bar = |model: &UiModel<Line>| {
            model
                .scene(LayoutClass::Compact)
                .chat
                .expect("chat")
                .compose
                .recording
        };
        assert_eq!(bar(&model), None, "no recording, no bar");
        assert_eq!(
            feed(&mut model, Input::Press { now: 0 }),
            alloc::vec![
                UiEffect::Recording {
                    peer,
                    effect: Effect::Haptic(recording::Haptic::Start)
                },
                UiEffect::Recording {
                    peer,
                    effect: Effect::Start
                },
            ]
        );
        // Three seconds rising and a quiet one.
        for w in 1..=80u64 {
            let rms = match w {
                ..=60 => u16::try_from(w * 200).unwrap_or(0),
                _ => 40,
            };
            feed(&mut model, Input::Level { now: w * 50, rms });
        }
        let held = bar(&model).expect("the bar while held");
        assert_eq!(held.phase.word(), "held");
        assert!(held.pulsing);
        assert_eq!(held.timer.words.resolve(Lang::En), "REC 00:04");
        assert!(!held.timer.amber);
        assert_eq!(held.wave.levels.len(), LIVE_WINDOWS);
        assert_eq!(
            held.wave.levels.as_slice(),
            model.recording.track.live(LIVE_WINDOWS),
            "the newest windows, the newest at the right"
        );
        assert!(held.wave.moving);
        let cancel = held.cancel.as_ref().expect("the hint");
        assert_eq!(cancel.words.resolve(Lang::En), "Slide left to cancel");
        assert_eq!(
            held.held.as_ref().map(|h| h.id.resource()),
            Some("record".into()),
            "the finger stays on the microphone"
        );
        let lock = held.lock.as_ref().expect("the chip");
        assert!(!lock.closed);
        assert!(held.buttons.is_empty());
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(
            scene
                .action(NodeId::Rec(Verb::Lock))
                .map(|a| a.intent.clone()),
            Some(Intent::Recording {
                peer,
                verb: Verb::Lock
            })
        );
        let json = scene.to_json();
        let rec = &json["chat"]["compose"]["recording"];
        assert_eq!(rec["phase"], "held");
        assert_eq!(rec["timer"]["words"]["args"][0], "00:04", "{rec}");
        assert_eq!(rec["lock"]["id"], "rec_lock");

        // Slid past the distance: the hint says release to cancel.
        feed(&mut model, Input::Slide { dx: -120, dy: 0 });
        let armed = bar(&model).and_then(|b| b.cancel).expect("hint");
        assert!(armed.armed);
        assert_eq!(armed.words.resolve(Lang::De), "Loslassen zum Abbrechen");
        // Back, then the chip: locked.
        feed(&mut model, Input::Slide { dx: 0, dy: 0 });
        feed(&mut model, Input::Verb(Verb::Lock));
        let locked = bar(&model).expect("the bar locked");
        assert!(locked
            .lock
            .as_ref()
            .is_some_and(|l| l.closed && l.action.is_none()));
        assert_eq!(locked.cancel, None);
        let ids: Vec<String> = locked.buttons.iter().map(|b| b.id.resource()).collect();
        assert_eq!(ids, ["rec_discard", "rec_pause", "rec_send"]);
        feed(&mut model, Input::Verb(Verb::Pause));
        let paused = bar(&model).expect("the bar paused");
        assert!(!paused.wave.moving && !paused.pulsing);
        assert_eq!(
            paused.wave.levels,
            model.recording.track.bars(LIVE_WINDOWS, Fold::Peak),
            "the whole recording, standing"
        );
        let ids: Vec<String> = paused.buttons.iter().map(|b| b.id.resource()).collect();
        assert_eq!(ids, ["rec_discard", "rec_play", "rec_resume", "rec_send"]);

        // Send: the platform finishes, hands the recording over, and the
        // bubble draws the track.
        assert_eq!(
            feed(&mut model, Input::Verb(Verb::Send)),
            alloc::vec![UiEffect::Recording {
                peer,
                effect: Effect::Finish
            }]
        );
        assert_eq!(bar(&model), None, "the field is back");
        let track = model.recording.track.clone();
        assert_eq!(track.len(), 80);
        super::super::update(
            &mut model,
            UiEvent::SendVoice {
                peer,
                data: b"OggS".to_vec(),
            },
        );
        let key = model.conversations[0].messages.last().map(|m| m.activity);
        let key = key.expect("the sent one");
        let scene = model.scene(LayoutClass::Compact);
        let voice = scene
            .bubble(NodeId::Bubble(key))
            .and_then(|n| n.voice.clone())
            .expect("its voice node");
        assert_eq!(voice.levels, track.bars(BARS, Fold::Peak));
        assert!(voice.levels.len() == BARS && voice.levels[0] < voice.levels[20]);

        // A received one the shell decoded draws the decoded sound's track.
        let decoded = recording::LevelTrack::of(alloc::vec![10, 20, 90, 30]);
        super::super::update(
            &mut model,
            UiEvent::VoiceTrack {
                peer,
                key: 1_000,
                track: decoded.clone(),
            },
        );
        assert_eq!(model.voice_track(peer, 1_000), Some(&decoded));
    }

    /// Nothing heard for two seconds: the line under the waveform, its
    /// way to the permission, and the dot stands.
    #[test]
    fn a_silent_microphone_is_said() {
        use super::super::recording::{Effect, Input};
        use super::super::UiEffect;
        let mut model = model_with(&[(false, 1_793_350_800)]);
        let peer = [0x42; 16];
        model.selected = 0;
        super::super::update(
            &mut model,
            UiEvent::Record {
                peer,
                input: Input::Press { now: 0 },
            },
        );
        for w in 1..=40u64 {
            super::super::update(
                &mut model,
                UiEvent::Record {
                    peer,
                    input: Input::Level {
                        now: w * 50,
                        rms: 0,
                    },
                },
            );
        }
        let scene = model.scene(LayoutClass::Compact);
        let bar = scene
            .chat
            .as_ref()
            .and_then(|c| c.compose.recording.clone())
            .expect("the bar");
        assert!(!bar.pulsing);
        let notice = bar.notice.as_ref().expect("the line");
        assert_eq!(
            notice.words.resolve(Lang::En),
            "Nothing heard. Is the microphone allowed?"
        );
        let ask = Intent::Recording {
            peer,
            verb: Verb::AskMicrophone,
        };
        assert_eq!(
            scene.action(NodeId::RecNotice).map(|a| a.intent.clone()),
            Some(ask)
        );
        assert_eq!(
            super::super::update(
                &mut model,
                UiEvent::Record {
                    peer,
                    input: Input::Verb(Verb::AskMicrophone)
                }
            ),
            alloc::vec![UiEffect::Recording {
                peer,
                effect: Effect::AskMicrophone
            }]
        );
        // A tap is said as the hint to hold, and nothing goes.
        let mut model = model_with(&[(false, 1_793_350_800)]);
        model.selected = 0;
        super::super::update(
            &mut model,
            UiEvent::Record {
                peer,
                input: Input::Press { now: 0 },
            },
        );
        super::super::update(
            &mut model,
            UiEvent::Record {
                peer,
                input: Input::Release { now: 200 },
            },
        );
        assert_eq!(model.conversations[0].messages.len(), 1);
        assert!(model
            .toasts
            .iter()
            .any(|t| t.text.starts_with("Hold the microphone")));
    }
}
