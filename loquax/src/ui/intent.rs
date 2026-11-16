//! The user's verbs as one enum (1217, section 3.3 of
//! `attachments/1217-loquax-one-ui-contract.md`): what a key, a click, a
//! tap or the phone's facade asks the model to do.
//!
//! Before this there were four spellings of them: the facade's
//! `schema::Request` (25 arms), the semantic arms of [`super::UiEvent`]
//! (13), the palette's [`Command`] (31) and the window's `gui::Message`
//! (18 user arms). Each maps onto an [`Intent`] here; the keymap's
//! [`keys::Action`] does too ([`Intent::from_action`]), and a scene node's
//! [`super::scene::Action`] carries the intent a tap on it raises. Slice
//! one decodes the facade's requests into intents at the boundary and
//! changes nothing behind it; the model folding intents itself is a later
//! slice.
//!
//! A verb of the palette or a key acts on what is open or under the
//! cursor, a verb of a tap or the facade on a named peer or message:
//! [`Target`] and [`Switch`] carry the difference instead of two arms per
//! verb.
//!
//! | Old name | Intent |
//! |---|---|
//! | `Request::Open`, `UiEvent::Open`, `Message::Open`, `MapCmd::Open` | `OpenChat` |
//! | `Request::Close`, `UiEvent::Close`, `Message::ShowChats` | `CloseChat` |
//! | `keys::Action::Back`, `MapCmd::Close` (as Back) | `Back` |
//! | `Request::Send`, `UiEvent::Write`, `Command::Send`, `Message::Send` | `Send` |
//! | `Request::React`, `UiEvent::React`, `Message::React`, `Command::React` | `React` |
//! | `Request::Reply`, `UiEvent::Reply`, `Message::Reply` | `Reply` |
//! | `Message::CancelReply` | `CancelReply` |
//! | `Request::NoticeReply`, `UiEvent::NoticeReply` (1506) | `NoticeReply` |
//! | `Request::Forward`, `UiEvent::Forward` | `Forward` |
//! | `Request::SendAgain`, `UiEvent::SendAgain` (1232) | `SendAgain` |
//! | `Request::Photo`, `UiEvent::SendPhoto` (1220) | `SendPhoto` |
//! | `Request::PhotoSize`, `UiEvent::PhotoSize` (1220) | `PhotoSize` |
//! | (new) a tap on a photo, `UiEvent::ViewPhoto` (1220) | `ViewPhoto` |
//! | `Request::File`, `UiEvent::SendFile` (1221) | `SendFile` |
//! | (new) a tap on a file, `UiEvent::OpenFile` (1221) | `OpenFile` |
//! | `Request::Voice`, `UiEvent::SendVoice` (1222) | `SendVoice` |
//! | `Request::PlayVoice`, `UiEvent::PlayVoice`, `UiEvent::PauseVoice` (1222) | `PlayVoice`, `PauseVoice` |
//! | (new) the recording bar's buttons, `UiEvent::Record` (1276) | `Recording` |
//! | `Request::Delete`, `UiEvent::Delete`, `Message::Delete`, `Message::AskDelete`, `Command::DeleteMessage` | `Delete` (`confirmed` tells the asked from the asking) |
//! | `Request::Used`, `Message::Insert` | `Insert` |
//! | `Message::ToggleEmoji` | `Emoji` |
//! | `Request::Pull`, `Command::Fetch` | `Pull` |
//! | `Request::Front` | `Front` |
//! | `Request::Network`, `UiEvent::Network` (1298) | `Network` |
//! | `Request::Viewport` (1231) | `Viewport` |
//! | `Request::Name` | `Name` |
//! | `Request::Rename`, `UiEvent::Rename`, `Command::Rename` | `Rename` |
//! | `Request::Pin`, `Command::Pin`, `UiEvent::PeerState` (pinned) | `Pin` |
//! | `Request::Mute`, `UiEvent::PeerState` (muted) | `Mute` |
//! | `Request::Archive`, `Command::Archive`, `UiEvent::PeerState` (archived) | `Archive` |
//! | `Request::Block`, `Command::Block`, `Command::Unblock`, `UiEvent::PeerState` (blocked) | `Block` |
//! | `Request::Verify`, `UiEvent::Verify` | `Verify` |
//! | `Request::Unread`, `UiEvent::MarkUnread` | `MarkUnread` |
//! | `Request::Forget`, `UiEvent::DeleteConversation`, `Command::DeleteConversation` | `Forget` |
//! | `Request::Share`, `UiEvent::SharePosition` | `Share` |
//! | `Request::AddContact` | `AddContact` |
//! | `Message::NewConversation`, `Message::NewAddress`, `Message::NewGo` | `NewConversation` (the address field is the platform's text) |
//! | `Command::Quit` | `Quit` |
//! | `keys::Action::QuitHint` | `QuitHint` |
//! | `Command::Help` | `Help` |
//! | `Command::Focus{Conversations,Messages,Compose}` | `Focus` |
//! | `keys::Action::NextPane`, `PreviousPane` | `NextPane`, `PreviousPane` |
//! | `keys::Action::Palette` | `Palette` |
//! | `Command::{Next,Previous}Conversation`, `{Next,Previous}Unread` | the same names |
//! | `Command::Details`, `keys::Action::OpenMessage` | `Details`, `OpenMessage` |
//! | `Command::Redraw` | `Redraw` |
//! | `Command::SendOnEnter`, `Mouse` | the same names |
//! | `Command::Theme`, `Message::ToggleTheme` | `Theme` |
//! | `Command::Search`, `Message::Search` | `Search` (the query is the platform's text) |
//! | `keys::Action::CloseSearch`, `OpenResult`, `Match` | the same names |
//! | `Command::Map`, `Message::ShowMap` | `ShowMap` |
//! | `keys::Action::PeerOnMap`, a row's map pin, the chat bar's map | `OpenMapAt` |
//! | `keys::Action::Map(MapCmd)` (but `Open`, `Close`) | `Map` |
//! | `Message::Select` | `SelectPin` |
//! | `Command::LoadMapArea`, `MapDrawing` | the same names |
//! | `Command::Blocked`, `Archived` | `ShowBlocked`, `ShowArchived` |
//! | `Command::Import` | `Import` |
//! | `Command::MyCode`, a tap or click on the reader's avatar, the phone's profile activity (1443) | `Profile` |
//! | (new) the profile's "Share my address" (1443) | `ShareCode` |
//! | `Command::ReadReceipts`, the settings' row, `UiEvent::ReadReceipts` | `ReadReceipts` |
//! | the Maps page's last row (1290), `Request::DeleteRegions` | `DeleteAllRegions` (`confirmed` tells the asked from the asking) |
//! | the window's gear (`Message::Settings`), the phone's ⋮ menu, `Command::Settings`, `S` (1450) | `Settings` |
//! | the Advanced section's hub lines, `Query::RoomsHub` (1577) | `RoomsHub` |
//! | `keys::Action::Scroll`, `ScrollMessages` | the same names |
//! | `keys::Action::Edit`, `Message::Compose` | `Edit` |
//! | `keys::Action::Newline`, `ClearDraft` | the same names |
//! | `Request::Draft`, `UiEvent::Draft` | `Draft` |
//! | `keys::Action::Copy`, `Message::Copy` | `Copy` |
//! | `Message::Menu` | `Menu` |
//! | (new) a tap or click on a node | `Activate` |
//! | (new) a sheet or menu closed | `Dismiss` |
//! | (new) a tab | `Tab` |
//! | (new) "New group", a tick, Next, Create, Join, the member sheet, Leave, Remove (1307), `Command::{NewGroup,JoinGroup,Members,LeaveGroup,RemoveMember}` | `Group` |
//!
//! Not intents: the facade's `Link`, `Unlink`, `NotNow` and `Relink` (the
//! phone's address book, platform glue), and the window's arms that are
//! not the user's (`Seen`, `Painted`, `MapSize`, `Scale`, `Window`, the
//! pointer's raw motion, `WheelRest`, `Prefetched`, `SystemTheme`).

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::telemetry::Location;
use leviculum_ui::commands::Command;
use leviculum_ui::keymap::ScrollCmd;

use super::groups::Verb;
use super::scene::{NodeId, TabId};
use super::strings::StrId;
use crate::keys::{self, MapCmd};

/// Which conversation a verb acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The open one: the palette's and the keys' verbs.
    Open,
    /// The named peer's: a tap on its row, the facade.
    Peer([u8; 16]),
}

/// Which message a verb acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Messages {
    /// The one under the cursor.
    Cursor,
    /// These of `peer`'s conversation, by their keys on this device.
    Keys { peer: [u8; 16], keys: Vec<u64> },
}

/// A yes-or-no verb: set, clear, or swap what it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
    Toggle,
}

impl Switch {
    pub fn of(on: bool) -> Self {
        if on {
            Switch::On
        } else {
            Switch::Off
        }
    }

    /// The value it leaves when the value now is `now`.
    pub fn apply(self, now: bool) -> bool {
        match self {
            Switch::On => true,
            Switch::Off => false,
            Switch::Toggle => !now,
        }
    }
}

/// Where the focus goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Messages,
    Compose,
}

/// What the user asks.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    // Navigation.
    /// A tab of the bar.
    Tab(TabId),
    /// A verb of the hosted browser in the Browse tab (1337): lnomad's own
    /// intent, carried, never one of Loquax's arms. A frontend hands it to
    /// the model as [`super::UiEvent::Browse`].
    Browse(lnomad::Intent),
    /// A verb of the Explore tab (1578): the search field, the order, a
    /// page's row, its bookmark, a room, a hub. A frontend hands it to the
    /// model as [`super::UiEvent::Explore`].
    Explore(super::explore::ExploreEvent),
    /// Back out one level: compose to messages to the list, an overlay
    /// closes, the map gives the conversation back.
    Back,
    /// Open `peer`'s conversation; its unread are read.
    OpenChat {
        peer: [u8; 16],
    },
    /// Leave the open conversation for the list.
    CloseChat,
    /// The map, at `peer`'s pin when one is named.
    ShowMap {
        peer: Option<[u8; 16]>,
    },
    /// The map centred on a peer's newest fix with its card open (1224):
    /// the open conversation's peer from a key, the named one from a
    /// row's pin or the chat's bar. Back from it goes to the list.
    OpenMapAt {
        peer: Target,
    },
    /// A tap or a click on a scene node: the node's own intent.
    Activate(NodeId),
    /// A long press or a right click on a node, or on none: its menu.
    Menu(Option<NodeId>),
    /// The open sheet, menu or dialog closes.
    Dismiss,
    Focus(Focus),
    NextPane,
    PreviousPane,
    Palette,
    Help,
    Quit,
    QuitHint,
    Redraw,

    // Writing.
    /// Send `body` to the conversation, or what compose holds.
    Send {
        to: Target,
        body: Option<String>,
    },
    /// Hand the key to the text being edited.
    Edit,
    Newline,
    ClearDraft,
    /// What the conversation's text field holds now, and the message a
    /// reply being written answers (1219): kept as its draft.
    Draft {
        to: Target,
        text: String,
        reply: Option<[u8; 32]>,
    },
    /// Put `text` into compose, an emoji from the picker.
    Insert(String),
    /// The emoji picker, open or closed.
    Emoji,
    React {
        to: Target,
        /// The message's LXMF id; none for the one under the cursor.
        target: Option<[u8; 32]>,
        content: Option<String>,
    },
    /// Reply to the message `target`: with `body` it is sent, without it
    /// compose starts the reply.
    Reply {
        to: Target,
        target: [u8; 32],
        body: Option<String>,
    },
    CancelReply,
    /// A reply typed into the notification of `peer`'s messages (1506):
    /// every message up to `key`, the newest the notification showed, is
    /// read, and `body` goes to `peer` with the receipt on it.
    NoticeReply {
        peer: [u8; 16],
        key: u64,
        body: String,
    },
    Forward {
        from: [u8; 16],
        key: u64,
        to: [u8; 16],
    },
    /// Send the message `key` of `peer`'s conversation, which was not
    /// sent, again: what its red mark does (1232).
    SendAgain {
        peer: [u8; 16],
        key: u64,
    },
    /// Show the photo of the message `key` of `peer`'s conversation whole:
    /// a tap on its bubble (1220).
    ViewPhoto {
        peer: [u8; 16],
        key: u64,
    },
    /// Send the picture the user picked to `peer` (1220): the paperclip's
    /// "Photo". `data` is the picture as picked; the shell scales it by
    /// the reader's [`Intent::PhotoSize`] before it goes.
    SendPhoto {
        peer: [u8; 16],
        data: Vec<u8>,
    },
    /// The reader's photo size: the settings' row (1220).
    PhotoSize(super::photos::PhotoSize),
    /// The compose bar's plus (1270): the attachment sheet opens.
    Attach {
        peer: [u8; 16],
    },
    /// The compose bar's position button, and the plus sheet's Position
    /// row (1270): the position sheet opens.
    Locate {
        peer: [u8; 16],
    },
    /// The compose bar's camera (1270): the platform takes a photo and
    /// answers with [`Intent::StagePhoto`].
    Camera {
        peer: [u8; 16],
    },
    /// The photo the camera took for `peer`, as taken (1270): the shell
    /// scales and strips it as [`Intent::SendPhoto`]'s and stages it in
    /// the bar, where Send takes it with the caption. Empty in the
    /// scene's action, which the platform fills.
    StagePhoto {
        peer: [u8; 16],
        data: Vec<u8>,
    },
    /// The staged photo's cross.
    Unstage {
        peer: [u8; 16],
    },
    /// The open sheet goes.
    CloseSheet,
    /// The position sheet's "Send my position now" (1270): the own fix
    /// goes to `peer` as a position report.
    ShareFix {
        peer: [u8; 16],
    },
    /// One of the position sheet's share rows (1273): a share of `span`
    /// starts for `peer`'s chat.
    StartShare {
        peer: [u8; 16],
        span: super::share::Span,
    },
    /// A running share's Stop: the chat's chip, Settings, Privacy, the
    /// phone's notification.
    StopShare {
        peer: [u8; 16],
    },
    /// The position sheet without a fix: the platform turns location on
    /// (the 1201 permission flow) and answers with [`Intent::OwnFix`].
    LocationOn,
    /// The position sheet's entry on a shell without a receiver (1371):
    /// without `text` the shell opens its field, with it the text is the
    /// own fix ([`super::UiEvent::EnterPosition`]).
    EnterPosition {
        text: Option<String>,
    },
    /// The position sheet's "map centre" row (1371).
    FixAtMapCentre,
    /// The platform's own fix, or none.
    OwnFix {
        location: Option<Location>,
    },
    /// Send the file the user picked to `peer` (1221): the paperclip's
    /// "File", its name and bytes as picked. `size` is the whole file's
    /// bytes where the shell stopped reading one too large to go, else 0.
    SendFile {
        peer: [u8; 16],
        name: String,
        data: Vec<u8>,
        size: u64,
    },
    /// Save and open the `index`th file of the message `key` of `peer`'s
    /// conversation: a tap on its row (1221).
    OpenFile {
        peer: [u8; 16],
        key: u64,
        index: usize,
    },
    /// Send the recording the microphone made to `peer` (1222): the
    /// compose bar's button, held and released. Mono 16-bit PCM at `rate`
    /// as the platform recorded it; the shell codes it
    /// (`crate::voice::encode`). Empty in the scene's action, which the
    /// platform fills.
    SendVoice {
        peer: [u8; 16],
        pcm: Vec<i16>,
        rate: u32,
    },
    /// A button of `peer`'s recording bar (1276): lock, pause, resume,
    /// listen, send, discard, or the way to the microphone's permission.
    /// The gestures on the microphone are not intents but the platform's
    /// input ([`super::recording::Input`]).
    Recording {
        peer: [u8; 16],
        verb: super::recording::Verb,
    },
    /// Play the voice message `key` of `peer`'s conversation: a tap on its
    /// play button (1222).
    PlayVoice {
        peer: [u8; 16],
        key: u64,
    },
    /// Stop it: a tap on the pause button of the one that plays.
    PauseVoice {
        peer: [u8; 16],
        key: u64,
    },
    /// Delete messages: `confirmed` once the user said yes, else ask.
    Delete {
        messages: Messages,
        confirmed: bool,
    },
    Copy,
    Share {
        peer: [u8; 16],
        location: Location,
    },

    // The reader's word on a peer.
    Pin {
        peer: Target,
        on: Switch,
    },
    /// Mute for `secs`, for ever below zero, unmute at zero.
    Mute {
        peer: Target,
        secs: i64,
    },
    Archive {
        peer: Target,
        on: Switch,
    },
    Block {
        peer: Target,
        on: Switch,
    },
    Verify {
        peer: Target,
        on: Switch,
    },
    MarkUnread {
        peer: Target,
        on: Switch,
    },
    /// A name of the reader's own; none asks for it, the empty one clears.
    Rename {
        peer: Target,
        name: Option<String>,
    },
    /// Delete the conversation, after asking unless `confirmed`; from every
    /// device of the home when `everywhere`.
    Forget {
        peer: Target,
        everywhere: bool,
        confirmed: bool,
    },
    AddContact {
        lxma: String,
        name: Option<String>,
    },
    /// A verb on groups (1307): "New group" and its picker, Join on an
    /// invitation, the member sheet, Leave, Remove. A frontend hands it to
    /// the model as [`super::UiEvent::Group`].
    Group(Verb),
    NewConversation,
    /// The reader's profile (1443): name, address, the contact code's QR,
    /// the mailbox, the devices. What a tap on the avatar raises on every
    /// target, and `my code` in the palette.
    Profile,
    /// The profile's "Share my address": the contact code to the phone's
    /// share sheet, the window's or the terminal's clipboard.
    ShareCode,
    /// The name recipients see.
    Name(String),

    // The list, the search, the messages.
    Scroll(ScrollCmd),
    ScrollMessages(ScrollCmd),
    NextConversation,
    PreviousConversation,
    NextUnread,
    PreviousUnread,
    /// Open the search, or put the cursor back in its field.
    Search,
    CloseSearch,
    OpenResult,
    /// The next match (`true`) or the previous.
    Match(bool),
    OpenMessage,
    Details,
    ShowBlocked,
    ShowArchived,

    // The map.
    Map(MapCmd),
    SelectPin([u8; 16]),
    LoadMapArea,
    MapDrawing,

    // Mail and settings.
    Pull,
    /// The app came to the front, or left it.
    Front(bool),
    /// The platform's network changed (1298): a fact the platform says,
    /// never a button.
    Network {
        kind: crate::home::NetworkKind,
        changed: bool,
    },
    /// These bubbles of `peer`'s chat are on screen, at least half of
    /// each (1231): the core decides from it what was read.
    Viewport {
        peer: [u8; 16],
        keys: Vec<u64>,
    },
    Theme,
    SendOnEnter,
    Mouse,
    Import,
    /// The switch "Send read receipts" (1233): set, or toggled.
    ReadReceipts {
        on: Switch,
    },
    /// The Maps page's last row (1290): delete every downloaded region,
    /// after asking unless `confirmed`. The autoload stays as it is.
    DeleteAllRegions {
        confirmed: bool,
    },
    /// The settings (1450): the list of the nine sections with `None`,
    /// one of them opened with its section (`super::settings`).
    Settings(Option<super::settings::Section>),
    /// The public rooms' hub (1577), a line of the Advanced section: a
    /// shipped hub, none, or another to paste.
    RoomsHub(super::rooms::HubPick),
}

impl Intent {
    /// What a key of the table asks. Every action is one: the keymap has no
    /// action an intent cannot carry.
    pub fn from_action(action: keys::Action) -> Self {
        match action {
            keys::Action::Run(command) => Self::from(command),
            keys::Action::Scroll(cmd) => Intent::Scroll(cmd),
            keys::Action::ScrollMessages(cmd) => Intent::ScrollMessages(cmd),
            keys::Action::Newline => Intent::Newline,
            keys::Action::Edit => Intent::Edit,
            keys::Action::ClearDraft => Intent::ClearDraft,
            keys::Action::Back => Intent::Back,
            keys::Action::NextPane => Intent::NextPane,
            keys::Action::PreviousPane => Intent::PreviousPane,
            keys::Action::Palette => Intent::Palette,
            keys::Action::QuitHint => Intent::QuitHint,
            keys::Action::Copy => Intent::Copy,
            // The copy mode is the terminal's cursor on the text (1475); a
            // target that selects with the pointer copies the message.
            keys::Action::CopyMode => Intent::Copy,
            keys::Action::OpenResult => Intent::OpenResult,
            keys::Action::Match(next) => Intent::Match(next),
            keys::Action::CloseSearch => Intent::CloseSearch,
            keys::Action::Map(cmd) => Intent::Map(cmd),
            keys::Action::OpenMessage => Intent::OpenMessage,
            keys::Action::PeerOnMap => Intent::OpenMapAt { peer: Target::Open },
            // The recording's keys are the terminal's (1276): they resolve
            // in its compose pane alone, where a key is the field's, and a
            // window or a phone records with the pointer.
            keys::Action::Record(_) => Intent::Edit,
        }
    }

    /// The catalogue's label for it: a menu entry, a palette row, the
    /// accessibility label of a button that raises it. Exhaustive, so an
    /// intent without a label does not build.
    pub fn label(&self) -> StrId {
        match self {
            Intent::Tab(TabId::Chats) => StrId::TabChats,
            Intent::Tab(TabId::Map) => StrId::Map,
            Intent::Tab(TabId::Browse) | Intent::Browse(_) => StrId::TabBrowse,
            Intent::Tab(TabId::Rooms) => StrId::TabRooms,
            Intent::Tab(TabId::Explore) | Intent::Explore(_) => StrId::TabExplore,
            Intent::Back => StrId::Back,
            Intent::OpenChat { .. } => StrId::Open,
            Intent::CloseChat => StrId::Close,
            Intent::ShowMap { .. } => StrId::Map,
            // A key's verb says whose; a named peer's button is the map
            // (the chat bar's, 1224; the row's pin says its own words).
            Intent::OpenMapAt { peer: Target::Open } => StrId::PeerOnMap,
            Intent::OpenMapAt { .. } => StrId::Map,
            Intent::Activate(_) => StrId::Open,
            Intent::Menu(_) => StrId::Menu,
            Intent::Dismiss => StrId::Dismiss,
            Intent::Focus(Focus::List) => StrId::FocusList,
            Intent::Focus(Focus::Messages) => StrId::FocusMessages,
            Intent::Focus(Focus::Compose) => StrId::FocusCompose,
            Intent::NextPane => StrId::NextPane,
            Intent::PreviousPane => StrId::PreviousPane,
            Intent::Palette => StrId::Palette,
            Intent::Help => StrId::Help,
            Intent::Quit => StrId::Quit,
            Intent::QuitHint => StrId::QuitHint,
            Intent::Redraw => StrId::Redraw,
            Intent::Send { .. } => StrId::Send,
            Intent::Edit => StrId::Edit,
            Intent::Newline => StrId::Newline,
            Intent::ClearDraft => StrId::ClearDraft,
            Intent::Draft { .. } => StrId::Draft,
            Intent::Insert(_) => StrId::Insert,
            Intent::Emoji => StrId::Emoji,
            Intent::React { .. } => StrId::React,
            Intent::Reply { .. } => StrId::ActionReply,
            Intent::CancelReply => StrId::CancelReply,
            Intent::NoticeReply { .. } => StrId::ActionReply,
            Intent::Forward { .. } => StrId::ActionForward,
            Intent::SendAgain { .. } => StrId::SendAgain,
            Intent::ViewPhoto { .. } => StrId::ViewPhoto,
            Intent::SendPhoto { .. } => StrId::AttachPhoto,
            Intent::PhotoSize(_) => StrId::PhotoSize,
            Intent::Attach { .. } => StrId::Attach,
            Intent::Locate { .. } => StrId::Locate,
            Intent::Camera { .. } => StrId::Camera,
            Intent::StagePhoto { .. } => StrId::Camera,
            Intent::Unstage { .. } => StrId::Unstage,
            Intent::CloseSheet => StrId::Dismiss,
            Intent::ShareFix { .. } => StrId::ShareNow,
            Intent::StartShare { span, .. } => match span {
                super::share::Span::Minutes15 => StrId::ShareFor15,
                super::share::Span::Hour1 => StrId::ShareFor1h,
                super::share::Span::Hours8 => StrId::ShareFor8h,
                super::share::Span::UntilStopped => StrId::ShareUntilStopped,
            },
            Intent::StopShare { .. } => StrId::ShareStop,
            Intent::LocationOn => StrId::LocationOn,
            Intent::EnterPosition { .. } => StrId::EnterPosition,
            Intent::FixAtMapCentre => StrId::MapCentre,
            Intent::OwnFix { .. } => StrId::Locate,
            Intent::SendFile { .. } => StrId::AttachFile,
            Intent::OpenFile { .. } => StrId::OpenFile,
            Intent::SendVoice { .. } => StrId::RecordVoice,
            Intent::PlayVoice { .. } => StrId::PlayVoice,
            Intent::Recording { verb, .. } => verb.label(),
            Intent::PauseVoice { .. } => StrId::PauseVoice,
            Intent::Delete { .. } => StrId::ActionDelete,
            Intent::Copy => StrId::ActionCopy,
            Intent::Share { .. } => StrId::Share,
            Intent::Pin {
                on: Switch::Off, ..
            } => StrId::Unpin,
            Intent::Pin { .. } => StrId::Pin,
            Intent::Mute { secs: 0, .. } => StrId::Unmute,
            Intent::Mute { .. } => StrId::Mute,
            Intent::Archive {
                on: Switch::Off, ..
            } => StrId::Unarchive,
            Intent::Archive { .. } => StrId::Archive,
            Intent::Block { .. } => StrId::Block,
            Intent::Verify { .. } => StrId::Verify,
            Intent::MarkUnread {
                on: Switch::Off, ..
            } => StrId::MarkRead,
            Intent::MarkUnread { .. } => StrId::MarkUnread,
            Intent::Rename { .. } => StrId::Rename,
            Intent::Forget { .. } => StrId::Forget,
            Intent::AddContact { .. } => StrId::AddContact,
            Intent::Group(verb) => match verb {
                Verb::New | Verb::Pick(_) | Verb::Name(_) => StrId::NewGroup,
                Verb::Next => StrId::GroupNext,
                Verb::Create => StrId::GroupCreate,
                Verb::Join { .. } => StrId::GroupJoin,
                Verb::Members { .. } => StrId::GroupMembers,
                Verb::Leave { .. } => StrId::GroupLeave,
                Verb::Remove { .. } => StrId::GroupRemove,
                Verb::Add { .. } => StrId::GroupAdd,
                Verb::Delete { .. } => StrId::GroupDelete,
                Verb::Again { .. } => StrId::GroupAgain,
                Verb::Close => StrId::Close,
            },
            Intent::NewConversation => StrId::NewConversation,
            Intent::Profile => StrId::ProfileTitle,
            Intent::ShareCode => StrId::ProfileShare,
            Intent::Name(_) => StrId::Name,
            Intent::Scroll(_) => StrId::Scroll,
            Intent::ScrollMessages(_) => StrId::ScrollMessages,
            Intent::NextConversation => StrId::NextConversation,
            Intent::PreviousConversation => StrId::PreviousConversation,
            Intent::NextUnread => StrId::NextUnread,
            Intent::PreviousUnread => StrId::PreviousUnread,
            Intent::Search => StrId::Search,
            Intent::CloseSearch => StrId::CloseSearch,
            Intent::OpenResult => StrId::OpenResult,
            Intent::Match(true) => StrId::NextMatch,
            Intent::Match(false) => StrId::PreviousMatch,
            Intent::OpenMessage => StrId::OpenMessage,
            Intent::Details => StrId::ActionInfo,
            Intent::ShowBlocked => StrId::Blocked,
            Intent::ShowArchived => StrId::Archived,
            Intent::Map(MapCmd::ZoomIn) => StrId::ZoomIn,
            Intent::Map(MapCmd::ZoomOut) => StrId::ZoomOut,
            Intent::Map(MapCmd::Pan(..)) => StrId::Pan,
            Intent::Map(MapCmd::Fit) => StrId::Fit,
            Intent::Map(MapCmd::FitWindow) => StrId::FitWindow,
            Intent::Map(MapCmd::Step(_)) => StrId::StepPin,
            Intent::Map(MapCmd::Open) => StrId::Open,
            Intent::Map(MapCmd::Close) => StrId::Close,
            Intent::Map(MapCmd::Drawing(_)) => StrId::MapDrawing,
            Intent::Map(MapCmd::GlyphTest) => StrId::GlyphTest,
            Intent::Map(MapCmd::TrackWindow(_)) => StrId::TrackWindow,
            Intent::Map(MapCmd::TimeZones) => StrId::TimeZones,
            Intent::SelectPin(_) => StrId::SelectPin,
            Intent::LoadMapArea => StrId::LoadMapArea,
            Intent::MapDrawing => StrId::MapDrawing,
            Intent::Pull => StrId::Pull,
            Intent::Front(_) => StrId::Front,
            // A fact of the platform as the front is, never a button.
            Intent::Network { .. } => StrId::Front,
            // A fact of the screen as the front is, never a button.
            Intent::Viewport { .. } => StrId::Front,
            Intent::Theme => StrId::Theme,
            Intent::SendOnEnter => StrId::SendOnEnter,
            Intent::Mouse => StrId::Mouse,
            Intent::Import => StrId::Import,
            Intent::ReadReceipts { .. } => StrId::ReadReceipts,
            Intent::DeleteAllRegions { .. } => StrId::DeleteAllRegionsYes,
            Intent::Settings(None) => StrId::Settings,
            Intent::Settings(Some(section)) => section.title(),
            Intent::RoomsHub(super::rooms::HubPick::None) => StrId::RoomsHubNone,
            Intent::RoomsHub(super::rooms::HubPick::Hub(_)) => StrId::RoomsHub,
            Intent::RoomsHub(super::rooms::HubPick::Paste) => StrId::RoomsHubPaste,
            Intent::RoomsHub(super::rooms::HubPick::More) => StrId::RoomsHubMore,
        }
    }
}

/// An intent's label for the shared scene's [`super::scene::Action`].
impl leviculum_ui::scene::Labelled for Intent {
    type Key = StrId;

    fn label(&self) -> StrId {
        Intent::label(self)
    }
}

impl From<Command> for Intent {
    /// The palette's command as an intent: on what is open, or under the
    /// cursor.
    fn from(command: Command) -> Self {
        match command {
            Command::Send => Intent::Send {
                to: Target::Open,
                body: None,
            },
            Command::Fetch => Intent::Pull,
            Command::Quit => Intent::Quit,
            Command::Help => Intent::Help,
            Command::FocusConversations => Intent::Focus(Focus::List),
            Command::FocusMessages => Intent::Focus(Focus::Messages),
            Command::FocusCompose => Intent::Focus(Focus::Compose),
            Command::NextConversation => Intent::NextConversation,
            Command::PreviousConversation => Intent::PreviousConversation,
            Command::NextUnread => Intent::NextUnread,
            Command::PreviousUnread => Intent::PreviousUnread,
            Command::Details => Intent::Details,
            Command::Redraw => Intent::Redraw,
            Command::SendOnEnter => Intent::SendOnEnter,
            Command::Mouse => Intent::Mouse,
            Command::Theme => Intent::Theme,
            Command::Search => Intent::Search,
            Command::DeleteMessage => Intent::Delete {
                messages: Messages::Cursor,
                confirmed: false,
            },
            Command::DeleteConversation => Intent::Forget {
                peer: Target::Open,
                everywhere: false,
                confirmed: false,
            },
            Command::Pin => Intent::Pin {
                peer: Target::Open,
                on: Switch::Toggle,
            },
            Command::Map => Intent::ShowMap { peer: None },
            Command::LoadMapArea => Intent::LoadMapArea,
            Command::MapDrawing => Intent::MapDrawing,
            Command::Browse => Intent::Tab(TabId::Browse),
            Command::Rooms => Intent::Tab(TabId::Rooms),
            Command::Explore => Intent::Tab(TabId::Explore),
            Command::Rename => Intent::Rename {
                peer: Target::Open,
                name: None,
            },
            Command::React => Intent::React {
                to: Target::Open,
                target: None,
                content: None,
            },
            Command::Block => Intent::Block {
                peer: Target::Open,
                on: Switch::On,
            },
            Command::Unblock => Intent::Block {
                peer: Target::Open,
                on: Switch::Off,
            },
            Command::Blocked => Intent::ShowBlocked,
            Command::Archive => Intent::Archive {
                peer: Target::Open,
                on: Switch::Toggle,
            },
            Command::Archived => Intent::ShowArchived,
            Command::Import => Intent::Import,
            Command::ReadReceipts => Intent::ReadReceipts { on: Switch::Toggle },
            Command::NewConversation => Intent::NewConversation,
            Command::MyCode => Intent::Profile,
            // No code yet: the frontend asks for it.
            Command::AddContact => Intent::AddContact {
                lxma: String::new(),
                name: None,
            },
            Command::NewGroup => Intent::Group(Verb::New),
            Command::JoinGroup => Intent::Group(Verb::Join {
                group: Target::Open,
            }),
            Command::Members => Intent::Group(Verb::Members {
                group: Target::Open,
            }),
            Command::LeaveGroup => Intent::Group(Verb::Leave {
                group: Target::Open,
                confirmed: false,
            }),
            Command::RemoveMember => Intent::Group(Verb::Remove {
                group: Target::Open,
                member: None,
                confirmed: false,
            }),
            Command::AddMembers => Intent::Group(Verb::Add {
                group: Target::Open,
            }),
            Command::DeleteGroup => Intent::Group(Verb::Delete {
                group: Target::Open,
                confirmed: false,
            }),
            Command::StartAgain => Intent::Group(Verb::Again {
                group: Target::Open,
            }),
            // The message under the cursor (1371): the model fills its id
            // and key when the palette runs them; the zero ones here name
            // the verb for its label.
            Command::Reply => Intent::Reply {
                to: Target::Open,
                target: [0; 32],
                body: None,
            },
            Command::SendAgain => Intent::SendAgain {
                peer: [0; 16],
                key: 0,
            },
            Command::Forward => Intent::Forward {
                from: [0; 16],
                key: 0,
                to: [0; 16],
            },
            Command::Mute => Intent::Mute {
                peer: Target::Open,
                secs: -1,
            },
            Command::Verify => Intent::Verify {
                peer: Target::Open,
                on: Switch::Toggle,
            },
            Command::MarkUnread => Intent::MarkUnread {
                peer: Target::Open,
                on: Switch::Toggle,
            },
            Command::PhotoSize => Intent::PhotoSize(super::photos::PhotoSize::Phone),
            Command::Position => Intent::EnterPosition { text: None },
            Command::SharePosition => Intent::ShareFix { peer: [0; 16] },
            Command::StopSharing => Intent::StopShare { peer: [0; 16] },
            Command::DeleteAllMaps => Intent::DeleteAllRegions { confirmed: false },
            Command::Settings => Intent::Settings(None),
            Command::SendPicture => Intent::SendPhoto {
                peer: [0; 16],
                data: Vec::new(),
            },
            Command::SendFile => Intent::SendFile {
                peer: [0; 16],
                name: String::new(),
                data: Vec::new(),
                size: 0,
            },
            Command::CodeFromPicture => Intent::NewConversation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::strings::Lang;

    /// Every key of the table reaches an intent, and every intent it
    /// reaches has a label in both languages (1217).
    #[test]
    fn every_key_reaches_an_intent_with_a_label() {
        let mut chords = 0;
        for row in keys::ROWS {
            for chord in row.chords() {
                let intent = Intent::from_action(chord.action);
                let label = intent.label();
                for lang in Lang::ALL {
                    assert!(
                        !label.text(lang).is_empty(),
                        "{}: {intent:?} has no {lang:?} label",
                        row.desc
                    );
                }
                chords += 1;
            }
        }
        assert!(chords > 50, "the table has {chords} chords");
    }

    /// Every palette command is an intent with a label: the palette offers
    /// nothing the scene cannot.
    #[test]
    fn every_palette_command_is_an_intent() {
        for entry in leviculum_ui::commands::COMMANDS {
            let intent = Intent::from(entry.command);
            for lang in Lang::ALL {
                assert!(!intent.label().text(lang).is_empty(), "{intent:?}");
            }
        }
    }

    #[test]
    fn a_switch_sets_clears_and_swaps() {
        assert!(Switch::On.apply(false));
        assert!(!Switch::Off.apply(true));
        assert!(Switch::Toggle.apply(false));
        assert!(!Switch::Toggle.apply(true));
        assert_eq!(Switch::of(true), Switch::On);
    }
}
