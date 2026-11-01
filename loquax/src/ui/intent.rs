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
//! | `Request::Forward`, `UiEvent::Forward` | `Forward` |
//! | `Request::SendAgain`, `UiEvent::SendAgain` (1232) | `SendAgain` |
//! | `Request::Photo`, `UiEvent::SendPhoto` (1220) | `SendPhoto` |
//! | `Request::PhotoSize`, `UiEvent::PhotoSize` (1220) | `PhotoSize` |
//! | (new) a tap on a photo, `UiEvent::ViewPhoto` (1220) | `ViewPhoto` |
//! | `Request::File`, `UiEvent::SendFile` (1221) | `SendFile` |
//! | (new) a tap on a file, `UiEvent::OpenFile` (1221) | `OpenFile` |
//! | `Request::Voice`, `UiEvent::SendVoice` (1222) | `SendVoice` |
//! | `Request::PlayVoice`, `UiEvent::PlayVoice`, `UiEvent::PauseVoice` (1222) | `PlayVoice`, `PauseVoice` |
//! | `Request::Delete`, `UiEvent::Delete`, `Message::Delete`, `Message::AskDelete`, `Command::DeleteMessage` | `Delete` (`confirmed` tells the asked from the asking) |
//! | `Request::Used`, `Message::Insert` | `Insert` |
//! | `Message::ToggleEmoji` | `Emoji` |
//! | `Request::Pull`, `Command::Fetch` | `Pull` |
//! | `Request::Front` | `Front` |
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
//! | `Command::ReadReceipts`, the settings' row, `UiEvent::ReadReceipts` | `ReadReceipts` |
//! | `keys::Action::Scroll`, `ScrollMessages` | the same names |
//! | `keys::Action::Edit`, `Message::Compose` | `Edit` |
//! | `keys::Action::Newline`, `ClearDraft` | the same names |
//! | `Request::Draft`, `UiEvent::Draft` | `Draft` |
//! | `keys::Action::Copy`, `Message::Copy` | `Copy` |
//! | `Message::Menu` | `Menu` |
//! | (new) a tap or click on a node | `Activate` |
//! | (new) a sheet or menu closed | `Dismiss` |
//! | (new) a tab | `Tab` |
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
    /// ([`crate::voice::encode`]). Empty in the scene's action, which the
    /// platform fills.
    SendVoice {
        peer: [u8; 16],
        pcm: Vec<i16>,
        rate: u32,
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
    NewConversation,
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
            keys::Action::OpenResult => Intent::OpenResult,
            keys::Action::Match(next) => Intent::Match(next),
            keys::Action::CloseSearch => Intent::CloseSearch,
            keys::Action::Map(cmd) => Intent::Map(cmd),
            keys::Action::OpenMessage => Intent::OpenMessage,
            keys::Action::PeerOnMap => Intent::OpenMapAt { peer: Target::Open },
        }
    }

    /// The catalogue's label for it: a menu entry, a palette row, the
    /// accessibility label of a button that raises it. Exhaustive, so an
    /// intent without a label does not build.
    pub fn label(&self) -> StrId {
        match self {
            Intent::Tab(TabId::Chats) => StrId::TabChats,
            Intent::Tab(TabId::Map) => StrId::Map,
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
            Intent::Forward { .. } => StrId::ActionForward,
            Intent::SendAgain { .. } => StrId::SendAgain,
            Intent::ViewPhoto { .. } => StrId::ViewPhoto,
            Intent::SendPhoto { .. } => StrId::AttachPhoto,
            Intent::PhotoSize(_) => StrId::PhotoSize,
            Intent::SendFile { .. } => StrId::AttachFile,
            Intent::OpenFile { .. } => StrId::OpenFile,
            Intent::SendVoice { .. } => StrId::RecordVoice,
            Intent::PlayVoice { .. } => StrId::PlayVoice,
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
            Intent::NewConversation => StrId::NewConversation,
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
            Intent::Map(MapCmd::Step(_)) => StrId::StepPin,
            Intent::Map(MapCmd::Open) => StrId::Open,
            Intent::Map(MapCmd::Close) => StrId::Close,
            Intent::Map(MapCmd::Drawing(_)) => StrId::MapDrawing,
            Intent::Map(MapCmd::GlyphTest) => StrId::GlyphTest,
            Intent::SelectPin(_) => StrId::SelectPin,
            Intent::LoadMapArea => StrId::LoadMapArea,
            Intent::MapDrawing => StrId::MapDrawing,
            Intent::Pull => StrId::Pull,
            Intent::Front(_) => StrId::Front,
            // A fact of the screen as the front is, never a button.
            Intent::Viewport { .. } => StrId::Front,
            Intent::Theme => StrId::Theme,
            Intent::SendOnEnter => StrId::SendOnEnter,
            Intent::Mouse => StrId::Mouse,
            Intent::Import => StrId::Import,
            Intent::ReadReceipts { .. } => StrId::ReadReceipts,
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
