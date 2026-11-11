//! The intent inventory (1443): every user action Loquax has, as the
//! [`super::intent::Intent`] it raises, and per target (the phone, the
//! window, the terminal, the M9) whether it is built, missing, or not
//! applicable with the reason. Lew, 2026-11-06: the avatar opened the
//! profile on the phone and did nothing in the window, though the targets
//! share their code; a table that says so per intent, kept true by a
//! test, is how the next such gap shows before a user finds it.
//!
//! A built cell names its evidence, and the guard (the tests below)
//! checks it: [`Via::Key`] a chord of the terminal's key table that
//! resolves to the intent (`Intent::from_action`), [`Via::Palette`] a
//! palette command that is the intent (`Intent::from`), [`Via::At`] a file
//! of the workspace and a token in it (the handler or the trigger). The
//! guard also refuses an intent without a row and a row without an
//! intent, and the page section `docs/src/concepts/loquax-ui.md`, "The
//! intent inventory", is [`markdown`] of this table, byte for byte.

use alloc::format;
use alloc::string::String;

/// How a built cell is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A chord of the terminal's key table.
    Key,
    /// A command of the palette.
    Palette,
    /// A file of the workspace and a token in it: the handler or the
    /// trigger.
    At(&'static str, &'static str),
}

/// One target's state for one intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Built(Via),
    /// Not built yet, and what it would take: the next orders.
    Missing(&'static str),
    /// Not applicable, and why.
    Na(&'static str),
}

/// One user action: the intent's name as `Debug` spells it, what the user
/// does, and its cell on each target.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub intent: &'static str,
    pub action: &'static str,
    pub phone: Cell,
    pub window: Cell,
    pub terminal: Cell,
    pub m9: Cell,
}

const SESSION: &str = "loquax-android/src/session.rs";
const JAVA_LIST: &str =
    "loquax-android/android/src/network/leviculum/loquax/ConversationsActivity.java";
const JAVA_CHAT: &str = "loquax-android/android/src/network/leviculum/loquax/ChatActivity.java";
const JAVA_PROFILE: &str =
    "loquax-android/android/src/network/leviculum/loquax/ProfileActivity.java";
const GUI: &str = "loquax/src/gui.rs";
const GUI_CHAT: &str = "loquax/src/gui/chat.rs";
const UI: &str = "loquax/src/ui.rs";
const TUI: &str = "loquax/src/tui.rs";
const KEYS: &str = "loquax/src/keys.rs";
const M9_KEYS: &str = "loquax-m9/src/keys.rs";
const M9_GROUP: &str = "loquax-m9/src/group.rs";
const M9_HOUSE: &str = "loquax-m9/src/house.rs";

const KEY: Cell = Cell::Built(Via::Key);
const PALETTE: Cell = Cell::Built(Via::Palette);

const fn at(file: &'static str, token: &'static str) -> Cell {
    Cell::Built(Via::At(file, token))
}

const fn missing(why: &'static str) -> Cell {
    Cell::Missing(why)
}

const fn na(why: &'static str) -> Cell {
    Cell::Na(why)
}

const TOUCH: &str = "a touch screen has no key for it";
const POINTER: &str = "the pointer does it";
const NO_POINTER: &str = "no pointer";
const ONE_PANE: &str = "one pane at a time";
const IME: &str = "the platform's text field does it";
const RADIO: &str = "too large for the radio";
const NO_CAMERA: &str = "no camera";
const BUFFER: &str = "compose is the model's own buffer";
const NO_RECEIVER: &str = "no position receiver";

/// The table, in the order of the enum.
pub const INVENTORY: &[Row] = &[
    Row {
        intent: "Tab",
        action: "a tab of the bar (Chats, Map, Explore)",
        phone: at(JAVA_LIST, "R.id.tab_map"),
        window: at(GUI, "Intent::Tab"),
        terminal: PALETTE,
        m9: missing("no key switches the tab"),
    },
    Row {
        intent: "Browse",
        action: "a verb of the hosted browser in the Browse tab",
        phone: at(SESSION, "Platform::Browse"),
        window: at(GUI, "Intent::Browse"),
        terminal: at(UI, "UiEvent::Browse(event)"),
        m9: missing("the M9 has no Browse tab"),
    },
    Row {
        intent: "Explore",
        action: "the Explore tab: search, order, open a page, bookmark it, open a room, pick a hub",
        phone: at(SESSION, "R::Explore {"),
        window: at(GUI, "Intent::Explore"),
        terminal: at(UI, "explore::key(model, key)"),
        m9: missing("the M9 has no third button; a key and a list screen for it (handheld page)"),
    },
    Row {
        intent: "Back",
        action: "back: the bar's arrow, Esc, the M9's back key, leaving the profile",
        phone: at(SESSION, "Intent::Back"),
        window: at(GUI, "Intent::Back"),
        terminal: KEY,
        m9: at(M9_KEYS, "Intent::Back"),
    },
    Row {
        intent: "OpenChat",
        action: "a tap or click on a conversation's row",
        phone: at(SESSION, "Intent::OpenChat"),
        window: at(GUI, "Intent::OpenChat"),
        terminal: at(UI, "Target::Conversation(index)"),
        m9: at(M9_KEYS, "verb: \"open\""),
    },
    Row {
        intent: "CloseChat",
        action: "leave the open chat for the list",
        phone: at(SESSION, "Intent::CloseChat"),
        window: at(GUI, "Intent::CloseChat"),
        terminal: na("the list stands beside the chat; Back moves the keys to it"),
        m9: na("Back moves the keys to the list"),
    },
    Row {
        intent: "ShowMap",
        action: "the map: its tab, the palette's map",
        phone: at(JAVA_LIST, "R.id.tab_map"),
        window: at(GUI, "Intent::ShowMap"),
        terminal: PALETTE,
        m9: missing("no key opens the map"),
    },
    Row {
        intent: "OpenMapAt",
        action: "a row's map pin, the chat bar's map: the peer on the map",
        phone: at(JAVA_LIST, "MapActivity"),
        window: at(GUI, "Intent::OpenMapAt"),
        terminal: KEY,
        m9: missing("no key shows the peer on the map"),
    },
    Row {
        intent: "Activate",
        action: "a tap or click on a scene node",
        phone: na("each View's click is its own listener"),
        window: at(GUI, "Intent::Activate"),
        terminal: at(TUI, "Intent::Activate"),
        m9: na(NO_POINTER),
    },
    Row {
        intent: "Menu",
        action: "a long press or right click on a row or a bubble: its verbs",
        phone: at(JAVA_CHAT, "LongPress"),
        window: at(GUI_CHAT, "Intent::Menu"),
        terminal: na("a row's verbs are the palette's"),
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "Dismiss",
        action: "a sheet or menu closed by a tap outside it",
        phone: na("the system's back closes a dialog"),
        window: at(GUI, "Intent::Dismiss"),
        terminal: na("Esc is Back"),
        m9: na("the back key is Back"),
    },
    Row {
        intent: "Focus",
        action: "focus the list, the messages or compose",
        phone: na(TOUCH),
        window: na(POINTER),
        terminal: PALETTE,
        m9: at(M9_KEYS, "Intent::Focus"),
    },
    Row {
        intent: "NextPane",
        action: "Tab: the next pane",
        phone: na(TOUCH),
        window: na(POINTER),
        terminal: KEY,
        m9: na(ONE_PANE),
    },
    Row {
        intent: "PreviousPane",
        action: "Shift-Tab: the previous pane",
        phone: na(TOUCH),
        window: na(POINTER),
        terminal: KEY,
        m9: na(ONE_PANE),
    },
    Row {
        intent: "Palette",
        action: "`:` opens the command palette",
        phone: na(TOUCH),
        window: missing("the window has no command palette"),
        terminal: KEY,
        m9: missing("no key opens the palette"),
    },
    Row {
        intent: "Help",
        action: "`?`: the keys' help",
        phone: na(TOUCH),
        window: at(GUI, "Intent::Help"),
        terminal: PALETTE,
        m9: missing("no key opens the help"),
    },
    Row {
        intent: "Quit",
        action: "quit",
        phone: na("Android ends apps"),
        window: na("the window's own close"),
        terminal: PALETTE,
        m9: na("the power key"),
    },
    Row {
        intent: "QuitHint",
        action: "Ctrl-c outside compose: say what quits",
        phone: na(TOUCH),
        window: na("the window's own close"),
        terminal: KEY,
        m9: na("the power key"),
    },
    Row {
        intent: "Redraw",
        action: "redraw the screen",
        phone: na("the platform redraws"),
        window: na("the platform redraws"),
        terminal: PALETTE,
        m9: na("the board redraws"),
    },
    Row {
        intent: "Send",
        action: "Send: the compose bar's button, Enter",
        phone: at(SESSION, "Intent::Send"),
        window: at(GUI, "Intent::Send"),
        terminal: PALETTE,
        m9: at(M9_KEYS, "Intent::Send"),
    },
    Row {
        intent: "Edit",
        action: "a key handed to the text being edited",
        phone: na(IME),
        window: na(IME),
        terminal: KEY,
        m9: at(M9_KEYS, "Intent::Edit"),
    },
    Row {
        intent: "Newline",
        action: "Alt-Enter: a new line in compose",
        phone: na(IME),
        window: na(IME),
        terminal: KEY,
        m9: missing("no key breaks the line"),
    },
    Row {
        intent: "ClearDraft",
        action: "Ctrl-u: clear compose",
        phone: na(IME),
        window: na(IME),
        terminal: KEY,
        m9: missing("no key clears compose"),
    },
    Row {
        intent: "Draft",
        action: "what the text field holds, kept as the draft",
        phone: at(SESSION, "Intent::Draft"),
        window: at(GUI, "UiEvent::Draft"),
        terminal: na(BUFFER),
        m9: na(BUFFER),
    },
    Row {
        intent: "Insert",
        action: "an emoji picked into compose",
        phone: at(SESSION, "Intent::Insert"),
        window: at(GUI, "Intent::Insert"),
        terminal: missing("no emoji picker"),
        m9: missing("no emoji picker"),
    },
    Row {
        intent: "Emoji",
        action: "the compose bar's emoji button: the picker",
        phone: at(JAVA_CHAT, "EmojiPicker"),
        window: at(GUI, "Intent::Emoji"),
        terminal: missing("no emoji picker"),
        m9: missing("no emoji picker"),
    },
    Row {
        intent: "React",
        action: "a reaction on a bubble",
        phone: at(SESSION, "Intent::React"),
        window: at(GUI, "Intent::React"),
        terminal: PALETTE,
        m9: missing("no key reacts"),
    },
    Row {
        intent: "Reply",
        action: "Reply on a bubble",
        phone: at(SESSION, "Intent::Reply"),
        window: at(GUI, "Intent::Reply"),
        terminal: PALETTE,
        m9: missing("no key replies"),
    },
    Row {
        intent: "CancelReply",
        action: "the reply bar's cross",
        phone: at(JAVA_CHAT, "R.id.reply_cancel"),
        window: at(GUI, "Intent::CancelReply"),
        terminal: missing("no way out of a reply but sending or clearing"),
        m9: missing("no key replies"),
    },
    Row {
        intent: "NoticeReply",
        action: "a reply typed into a message notification: read up to what it showed",
        phone: at(SESSION, "Intent::NoticeReply"),
        window: missing("the desktop notification carries no actions"),
        terminal: missing("the desktop notification carries no actions"),
        m9: na("the board posts no notifications"),
    },
    Row {
        intent: "Forward",
        action: "Forward on a bubble, then a chat",
        phone: at(SESSION, "Intent::Forward"),
        window: at(GUI, "Intent::Forward"),
        terminal: PALETTE,
        m9: missing("no key forwards"),
    },
    Row {
        intent: "SendAgain",
        action: "the red mark of a message not sent",
        phone: at(SESSION, "Intent::SendAgain"),
        window: at(GUI, "Intent::SendAgain"),
        terminal: PALETTE,
        m9: missing("no key sends again"),
    },
    Row {
        intent: "ViewPhoto",
        action: "a tap on a bubble's photo: the photo whole",
        phone: at(JAVA_CHAT, "PhotoActivity"),
        window: at(GUI, "Intent::ViewPhoto"),
        terminal: missing("no photo viewer"),
        m9: na(RADIO),
    },
    Row {
        intent: "SendPhoto",
        action: "the plus sheet's Photo: a picture picked and sent",
        phone: at(SESSION, "Intent::SendPhoto"),
        window: at(GUI, "Intent::SendPhoto"),
        terminal: PALETTE,
        m9: na(RADIO),
    },
    Row {
        intent: "PhotoSize",
        action: "the settings' photo size",
        phone: at(SESSION, "Query::PhotoSize"),
        window: at(GUI, "Intent::PhotoSize"),
        terminal: PALETTE,
        m9: na(RADIO),
    },
    Row {
        intent: "Attach",
        action: "the compose bar's plus: the attachment sheet",
        phone: at(SESSION, "Intent::Attach"),
        window: at(GUI, "Intent::Attach"),
        terminal: na("the palette names each attachment"),
        m9: na(RADIO),
    },
    Row {
        intent: "Locate",
        action: "the compose bar's position button: the position sheet",
        phone: at(SESSION, "Intent::Locate"),
        window: at(GUI, "Intent::Locate"),
        terminal: na("the palette names each position verb"),
        m9: missing("no key opens the position sheet"),
    },
    Row {
        intent: "Camera",
        action: "the compose bar's camera",
        phone: at(JAVA_CHAT, "CaptureProvider"),
        window: na(NO_CAMERA),
        terminal: na(NO_CAMERA),
        m9: na(NO_CAMERA),
    },
    Row {
        intent: "StagePhoto",
        action: "the camera's photo staged in the bar",
        phone: at(SESSION, "Intent::StagePhoto"),
        window: na(NO_CAMERA),
        terminal: na(NO_CAMERA),
        m9: na(NO_CAMERA),
    },
    Row {
        intent: "Unstage",
        action: "the staged photo's cross",
        phone: at(SESSION, "Intent::Unstage"),
        window: na(NO_CAMERA),
        terminal: na(NO_CAMERA),
        m9: na(NO_CAMERA),
    },
    Row {
        intent: "CloseSheet",
        action: "a sheet of the compose bar closed",
        phone: at(SESSION, "Intent::CloseSheet"),
        window: at(GUI, "Intent::CloseSheet"),
        terminal: na("the terminal opens no sheet"),
        m9: na("the M9 opens no compose sheet"),
    },
    Row {
        intent: "ShareFix",
        action: "the position sheet's \"Send my position now\"",
        phone: at(SESSION, "Intent::ShareFix"),
        window: at(GUI, "Intent::ShareFix"),
        terminal: PALETTE,
        m9: missing("no key shares the position"),
    },
    Row {
        intent: "StartShare",
        action: "a share row of the position sheet: 15 min, 1 h, 8 h, until stopped",
        phone: at(SESSION, "Intent::StartShare"),
        window: at(GUI, "Intent::StartShare"),
        terminal: missing("the palette shares once, never for a while"),
        m9: missing("no key shares the position"),
    },
    Row {
        intent: "StopShare",
        action: "a running share's Stop: the chat's chip, Settings, the notification",
        phone: at(SESSION, "Intent::StopShare"),
        window: at(GUI_CHAT, "fn share_chip"),
        terminal: PALETTE,
        m9: missing("no key shares the position"),
    },
    Row {
        intent: "LocationOn",
        action: "the position sheet without a fix: turn location on",
        phone: at(
            "loquax-android/android/src/network/leviculum/loquax/Locator.java",
            "ACCESS_FINE_LOCATION",
        ),
        window: na(NO_RECEIVER),
        terminal: na(NO_RECEIVER),
        m9: missing("the M9's GNSS is not wired to the sheet"),
    },
    Row {
        intent: "EnterPosition",
        action: "the position sheet's entry on a shell without a receiver",
        phone: na("the phone has a receiver"),
        window: at(GUI, "Intent::EnterPosition"),
        terminal: PALETTE,
        m9: missing("no key enters a position"),
    },
    Row {
        intent: "FixAtMapCentre",
        action: "the position sheet's \"map centre\" row",
        phone: na("the phone has a receiver"),
        window: at(GUI, "Intent::FixAtMapCentre"),
        terminal: missing("no \"map centre\" in the palette"),
        m9: missing("no key enters a position"),
    },
    Row {
        intent: "OwnFix",
        action: "the platform's own fix, or none",
        phone: at(SESSION, "Intent::OwnFix"),
        window: na(NO_RECEIVER),
        terminal: na(NO_RECEIVER),
        m9: missing("the M9's GNSS is not wired to the model"),
    },
    Row {
        intent: "SendFile",
        action: "the plus sheet's File: a file picked and sent",
        phone: at(SESSION, "Intent::SendFile"),
        window: at(GUI, "Intent::SendFile"),
        terminal: PALETTE,
        m9: na(RADIO),
    },
    Row {
        intent: "OpenFile",
        action: "a tap on a bubble's file: saved and opened",
        phone: at(SESSION, "Query::SaveFile"),
        window: at(GUI, "Intent::OpenFile"),
        terminal: missing("no way to save a file from a bubble"),
        m9: na(RADIO),
    },
    Row {
        intent: "SendVoice",
        action: "the microphone held and released: a voice message",
        phone: at(SESSION, "Intent::SendVoice"),
        window: at(GUI, "Want::Record"),
        terminal: at(KEYS, "Record(RecKey::Send)"),
        m9: na(RADIO),
    },
    Row {
        intent: "Recording",
        action: "a button of the recording bar: lock, pause, listen, send, discard",
        phone: at(SESSION, "Intent::Recording"),
        window: at(GUI, "Intent::Recording"),
        terminal: at(KEYS, "Record(RecKey::Pause)"),
        m9: na(RADIO),
    },
    Row {
        intent: "PlayVoice",
        action: "a voice message's play button",
        phone: at(SESSION, "Intent::PlayVoice"),
        window: at(GUI, "Intent::PlayVoice"),
        terminal: missing("no key plays a voice message"),
        m9: na(RADIO),
    },
    Row {
        intent: "PauseVoice",
        action: "a playing voice message's pause button",
        phone: at(SESSION, "Intent::PauseVoice"),
        window: at(GUI, "Intent::PauseVoice"),
        terminal: missing("no key plays a voice message"),
        m9: na(RADIO),
    },
    Row {
        intent: "Delete",
        action: "Delete on a bubble, asked, then confirmed",
        phone: at(SESSION, "Intent::Delete"),
        window: at(GUI, "Intent::Delete"),
        terminal: PALETTE,
        m9: missing("no key deletes a message"),
    },
    Row {
        intent: "Copy",
        action: "Copy on a bubble or a selection",
        phone: at(JAVA_CHAT, "setPrimaryClip"),
        window: at(GUI, "Intent::Copy"),
        terminal: KEY,
        m9: na("no clipboard"),
    },
    Row {
        intent: "Share",
        action: "a position shared from the map",
        phone: at(SESSION, "Intent::Share"),
        window: at(GUI, "Intent::Share {"),
        terminal: na("the palette's position verbs share"),
        m9: missing("no key shares the position"),
    },
    Row {
        intent: "Pin",
        action: "a row's Pin",
        phone: at(SESSION, "Intent::Pin"),
        window: at(GUI, "Intent::Pin"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "Mute",
        action: "a row's Mute",
        phone: at(SESSION, "Intent::Mute"),
        window: at(GUI, "Intent::Mute"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "Archive",
        action: "a row's Archive",
        phone: at(SESSION, "Intent::Archive"),
        window: at(GUI, "Intent::Archive"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "Block",
        action: "a contact's Block",
        phone: at(SESSION, "Intent::Block"),
        window: at(GUI, "Intent::Block"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "Verify",
        action: "a contact's Verify",
        phone: at(SESSION, "Intent::Verify"),
        window: at(GUI, "Intent::Verify"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "MarkUnread",
        action: "a row's Mark unread",
        phone: at(SESSION, "Intent::MarkUnread"),
        window: at(GUI, "Intent::MarkUnread"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "Rename",
        action: "a contact's Rename",
        phone: at(SESSION, "Intent::Rename"),
        window: at(GUI, "Intent::Rename"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "Forget",
        action: "a row's Delete chat, asked, then confirmed",
        phone: at(SESSION, "Intent::Forget"),
        window: at(GUI, "Intent::Forget"),
        terminal: PALETTE,
        m9: missing("no key opens a row's verbs"),
    },
    Row {
        intent: "AddContact",
        action: "a friend's code pasted or scanned: the contact",
        phone: at(SESSION, "Intent::AddContact"),
        window: at(GUI, "Intent::AddContact"),
        terminal: PALETTE,
        m9: missing("no way to add a contact"),
    },
    Row {
        intent: "Group",
        action: "New group, Join, the member sheet, Leave, Remove",
        phone: at(SESSION, "Intent::Group"),
        window: at(GUI, "Intent::Group"),
        terminal: PALETTE,
        m9: at(M9_GROUP, "Intent::Group"),
    },
    Row {
        intent: "NewConversation",
        action: "the bar's pencil: a new chat",
        phone: at(JAVA_LIST, "NewConversationActivity"),
        window: at(GUI, "Intent::NewConversation"),
        terminal: PALETTE,
        m9: missing("no way to start a chat"),
    },
    Row {
        intent: "Profile",
        action: "a tap or click on the reader's avatar, `P`, `my code`",
        phone: at(SESSION, "Intent::Profile"),
        window: at(GUI, "Intent::Profile"),
        terminal: KEY,
        m9: missing(
            "the house key leads to the settings (1561), which hold no profile: the M9 is a device of its home (super::settings::PLACES)",
        ),
    },
    Row {
        intent: "ShareCode",
        action: "the profile's \"Share my address\"",
        phone: at(JAVA_PROFILE, "R.id.profile_share"),
        window: at(GUI, "Intent::ShareCode"),
        terminal: at(UI, "return model.share_code();"),
        m9: na("no clipboard or share sheet"),
    },
    Row {
        intent: "Name",
        action: "the name recipients see",
        phone: at(SESSION, "Intent::Name"),
        window: missing("the profile shows the name, nothing sets it"),
        terminal: missing("the profile shows the name, nothing sets it"),
        m9: missing("the name is the pairing's"),
    },
    Row {
        intent: "Scroll",
        action: "the list's cursor up and down",
        phone: na("the list scrolls itself"),
        window: at(GUI, "Intent::Scroll"),
        terminal: KEY,
        m9: at(M9_KEYS, "Intent::Scroll"),
    },
    Row {
        intent: "ScrollMessages",
        action: "the messages' cursor from compose",
        phone: na("the chat scrolls itself"),
        window: na("the chat scrolls itself"),
        terminal: KEY,
        m9: na("Back leaves compose for the bubbles"),
    },
    Row {
        intent: "NextConversation",
        action: "the next conversation",
        phone: na(TOUCH),
        window: na(POINTER),
        terminal: PALETTE,
        m9: na("up and down move the list's cursor"),
    },
    Row {
        intent: "PreviousConversation",
        action: "the previous conversation",
        phone: na(TOUCH),
        window: na(POINTER),
        terminal: PALETTE,
        m9: na("up and down move the list's cursor"),
    },
    Row {
        intent: "NextUnread",
        action: "the next conversation with unread messages",
        phone: na(TOUCH),
        window: na(POINTER),
        terminal: PALETTE,
        m9: missing("no key jumps to the unread"),
    },
    Row {
        intent: "PreviousUnread",
        action: "the previous conversation with unread messages",
        phone: na(TOUCH),
        window: na(POINTER),
        terminal: PALETTE,
        m9: missing("no key jumps to the unread"),
    },
    Row {
        intent: "Search",
        action: "the bar's magnifier, `/`",
        phone: at(JAVA_LIST, "R.id.search"),
        window: at(GUI, "Intent::Search"),
        terminal: PALETTE,
        m9: missing("no key searches"),
    },
    Row {
        intent: "CloseSearch",
        action: "Esc in the search",
        phone: na("the system's back"),
        window: missing("the window's search field has no close"),
        terminal: KEY,
        m9: missing("no key searches"),
    },
    Row {
        intent: "OpenResult",
        action: "Enter on a search result",
        phone: na("a result is a row; a tap opens it"),
        window: na("a result is a row; a click opens it"),
        terminal: KEY,
        m9: missing("no key searches"),
    },
    Row {
        intent: "Match",
        action: "the next or previous match",
        phone: na("the phone lists the results"),
        window: na("the window lists the results"),
        terminal: KEY,
        m9: missing("no key searches"),
    },
    Row {
        intent: "OpenMessage",
        action: "Enter on a bubble: a position on the map, else its detail",
        phone: na("a tap on the bubble is its own"),
        window: na("a click on the bubble is its own"),
        terminal: KEY,
        m9: missing("no key opens a message"),
    },
    Row {
        intent: "Details",
        action: "a bubble's Info",
        phone: at(JAVA_CHAT, "Core.info("),
        window: at(GUI_CHAT, "Message::Intent(Intent::Details)"),
        terminal: PALETTE,
        m9: missing("no key opens a message"),
    },
    Row {
        intent: "ShowBlocked",
        action: "the blocked contacts",
        phone: at(
            "loquax-android/android/src/network/leviculum/loquax/ContactActivity.java",
            "blocked",
        ),
        window: missing("the window lists no blocked contacts"),
        terminal: PALETTE,
        m9: missing("no list of the blocked"),
    },
    Row {
        intent: "ShowArchived",
        action: "the archived chats",
        phone: at(JAVA_LIST, "R.string.archived"),
        window: missing("the window lists no archived chats"),
        terminal: PALETTE,
        m9: missing("no list of the archived"),
    },
    Row {
        intent: "Map",
        action: "zoom, pan, fit, step between pins, the map's drawing",
        phone: na("pinch and pan are the map View's"),
        window: at(GUI, "Intent::Map"),
        terminal: KEY,
        m9: missing("no key moves the map"),
    },
    Row {
        intent: "SelectPin",
        action: "a tap or click on a pin: its card",
        phone: at(
            "loquax-android/android/src/network/leviculum/loquax/MapView.java",
            "pin",
        ),
        window: at(GUI, "self.selected = press.pin"),
        terminal: at(UI, "Target::Pin(peer) => Some(peer)"),
        m9: missing("no key steps between pins"),
    },
    Row {
        intent: "LoadMapArea",
        action: "load the map's area here",
        phone: na("the map loads what it shows"),
        window: at(GUI, "Intent::LoadMapArea"),
        terminal: PALETTE,
        m9: na("the board carries its regions"),
    },
    Row {
        intent: "MapDrawing",
        action: "how the map is drawn",
        phone: na("the phone's map draws one way"),
        window: missing("the window offers no drawing"),
        terminal: PALETTE,
        m9: na("the board draws one way"),
    },
    Row {
        intent: "Pull",
        action: "fetch messages now",
        phone: at(SESSION, "Intent::Pull"),
        window: at(GUI_CHAT, "Message::Intent(Intent::Pull)"),
        terminal: PALETTE,
        m9: na("the board pulls on its own"),
    },
    Row {
        intent: "Front",
        action: "the app came to the front or left it",
        phone: at(SESSION, "Intent::Front"),
        window: at(GUI, "Want::Front"),
        terminal: na("a terminal is in front while it runs"),
        m9: na("the board's screen is always in front"),
    },
    Row {
        intent: "Network",
        action: "the platform's network changed",
        phone: at(SESSION, "Intent::Network"),
        window: na("the daemon follows the network"),
        terminal: na("the daemon follows the network"),
        m9: na("the board's radio is its network"),
    },
    Row {
        intent: "Viewport",
        action: "these bubbles are on screen: what was read",
        phone: at(SESSION, "Intent::Viewport"),
        window: at(GUI, "UiEvent::Viewport { peer, keys }"),
        terminal: na("the open conversation is read"),
        m9: na("the open conversation is read"),
    },
    Row {
        intent: "Theme",
        action: "light or dark",
        phone: na("follows the phone"),
        window: at(GUI, "Intent::Theme"),
        terminal: PALETTE,
        m9: na("the panel draws dark"),
    },
    Row {
        intent: "SendOnEnter",
        action: "whether Enter sends",
        phone: na(IME),
        window: missing("the window has no switch for Enter"),
        terminal: PALETTE,
        m9: na("Enter sends"),
    },
    Row {
        intent: "Mouse",
        action: "the mouse captured or the terminal's own selection",
        phone: na(NO_POINTER),
        window: na("the window's pointer is its own"),
        terminal: PALETTE,
        m9: na(NO_POINTER),
    },
    Row {
        intent: "Import",
        action: "import from another messenger",
        phone: missing("the phone imports nothing"),
        window: missing("the window imports nothing"),
        terminal: PALETTE,
        m9: na("the board takes its history from the home"),
    },
    Row {
        intent: "ReadReceipts",
        action: "the switch \"Send read receipts\"",
        phone: at(SESSION, "Query::Receipts"),
        window: at(GUI, "Intent::ReadReceipts"),
        terminal: PALETTE,
        m9: missing("the M9's settings have no read receipts row: the home signs them (super::settings::PLACES, Privacy)"),
    },
    Row {
        intent: "DeleteAllRegions",
        action: "the Maps page's Delete all",
        phone: at(SESSION, "Intent::DeleteAllRegions"),
        window: at(GUI, "Intent::DeleteAllRegions"),
        terminal: PALETTE,
        m9: na("the board carries its regions"),
    },
    Row {
        intent: "Settings",
        action: "the settings: the nine sections of super::settings::SECTIONS, whose own table says per section and target where it is drawn",
        phone: at(JAVA_LIST, "SettingsActivity.class"),
        window: at(GUI, "Intent::Settings"),
        terminal: KEY,
        m9: at(M9_HOUSE, "settings::screen(model, Target::M9, None)"),
    },
    Row {
        intent: "RoomsHub",
        action: "the public rooms' hub in Advanced: a shipped hub, none, or another pasted (1577)",
        phone: at(SESSION, "Query::RoomsHub"),
        window: at(GUI, "Intent::RoomsHub"),
        terminal: at(UI, "Intent::RoomsHub(rooms::HubPick::Paste)"),
        m9: missing("the M9 has no rooms: its settings hold no hub (super::settings::PLACES, Advanced)"),
    },
];

/// A cell as the page shows it.
fn said(cell: &Cell) -> String {
    match cell {
        Cell::Built(Via::Key) => String::from("built (key)"),
        Cell::Built(Via::Palette) => String::from("built (palette)"),
        Cell::Built(Via::At(file, _)) => {
            let name = file.rsplit('/').next().unwrap_or(file);
            format!("built (`{name}`)")
        }
        Cell::Missing(why) => format!("**missing**: {why}"),
        Cell::Na(why) => format!("n/a: {why}"),
    }
}

/// The table as the page's section has it, between its markers.
pub fn markdown() -> String {
    let mut out = String::from(
        "| intent | what the user does | phone | window | terminal | M9 |\n\
         |---|---|---|---|---|---|\n",
    );
    for row in INVENTORY {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            row.intent,
            row.action,
            said(&row.phone),
            said(&row.window),
            said(&row.terminal),
            said(&row.m9),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;
    use crate::ui::intent::Intent;
    use alloc::collections::BTreeSet;
    use alloc::vec::Vec;
    use std::path::PathBuf;

    /// The workspace's root, where the cells' files are named from.
    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    /// An intent's name as `Debug` spells it: up to its first field.
    fn name_of(intent: &Intent) -> String {
        let debug = format!("{intent:?}");
        debug
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The variants of `pub enum Intent`, read from its source: a new arm
    /// with no row fails here.
    fn variants() -> Vec<String> {
        let source =
            std::fs::read_to_string(root().join("loquax/src/ui/intent.rs")).expect("intent.rs");
        let start = source.find("pub enum Intent {").expect("the enum");
        let body = &source[start..];
        let end = body.find("\n}\n").expect("its end");
        let mut depth = 0i32;
        let mut names = Vec::new();
        for line in body[..end].lines().skip(1) {
            let line = line.trim();
            if line.starts_with("//") {
                continue;
            }
            if depth == 0 {
                let name: String = line.chars().take_while(|c| c.is_alphanumeric()).collect();
                if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                    names.push(name);
                }
            }
            depth += line.matches(['{', '(']).count() as i32;
            depth -= line.matches(['}', ')']).count() as i32;
        }
        names
    }

    /// The intents a chord of the terminal's key table raises.
    fn keyed() -> BTreeSet<String> {
        let mut names: BTreeSet<String> = keys::ROWS
            .iter()
            .flat_map(|row| row.chords())
            .map(|chord| name_of(&Intent::from_action(chord.action)))
            .collect();
        if !leviculum_ui::keymap::SCROLL_ROWS.is_empty() {
            names.insert(String::from("Scroll"));
        }
        names
    }

    /// The intents a palette command is.
    fn paletted() -> BTreeSet<String> {
        leviculum_ui::commands::COMMANDS
            .iter()
            .map(|entry| name_of(&Intent::from(entry.command)))
            .collect()
    }

    /// Every variant of `Intent` has one row, in the enum's order, and no
    /// row names an intent that is not one (1443).
    #[test]
    fn every_intent_has_its_row_in_the_inventory() {
        let rows: Vec<String> = INVENTORY.iter().map(|r| String::from(r.intent)).collect();
        assert_eq!(rows, variants());
        // The control: the reading of the enum finds the arms it must.
        assert!(variants().len() > 90, "{:?}", variants());
        assert!(variants().contains(&String::from("Profile")));
    }

    /// Every built cell is true: its key, its palette command, or its
    /// token in its file. A missing or n/a cell says why.
    #[test]
    fn every_built_cell_has_its_trigger_or_handler() {
        let (keyed, paletted) = (keyed(), paletted());
        let mut wrong = Vec::new();
        for row in INVENTORY {
            for (target, cell) in [
                ("phone", row.phone),
                ("window", row.window),
                ("terminal", row.terminal),
                ("m9", row.m9),
            ] {
                let fault = match cell {
                    Cell::Built(Via::Key) if !keyed.contains(row.intent) => {
                        Some(String::from("no chord raises it"))
                    }
                    Cell::Built(Via::Palette) if !paletted.contains(row.intent) => {
                        Some(String::from("no palette command is it"))
                    }
                    Cell::Built(Via::At(file, token)) => {
                        match std::fs::read_to_string(root().join(file)) {
                            Ok(text) if text.contains(token) => None,
                            Ok(_) => Some(format!("{file} has no {token:?}")),
                            Err(error) => Some(format!("{file}: {error}")),
                        }
                    }
                    Cell::Missing("") | Cell::Na("") => Some(String::from("no reason")),
                    _ => None,
                };
                if let Some(fault) = fault {
                    wrong.push(format!("{} on the {target}: {fault}", row.intent));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The arms of the phone's `session::decode`: each request's tag as
    /// its schema spells it on the wire (lowercase), and the arm's text.
    fn decoded_requests() -> Vec<(String, String)> {
        let source = std::fs::read_to_string(root().join(SESSION)).expect("session.rs");
        let start = source
            .find("pub fn decode(request: schema::Request)")
            .expect("the phone's decode");
        let body = &source[start..];
        let end = body.find("\n}\n").expect("its end");
        let mut arms: Vec<(String, String)> = Vec::new();
        for line in body[..end].lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("R::") {
                let tag: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
                arms.push((tag.to_lowercase(), String::new()));
            }
            if let Some((_, text)) = arms.last_mut() {
                text.push_str(line);
                text.push('\n');
            }
        }
        arms
    }

    /// The tags the app's Java puts in its requests: every `"t", "<tag>"`
    /// of its sources.
    fn java_tags() -> BTreeSet<String> {
        let dir = root().join("loquax-android/android/src/network/leviculum/loquax");
        let mut tags = BTreeSet::new();
        for entry in std::fs::read_dir(&dir).expect("the Java sources") {
            let path = entry.expect("an entry").path();
            if path.extension().is_none_or(|e| e != "java") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a Java source");
            for piece in text.split("\"t\", \"").skip(1) {
                let tag: String = piece.chars().take_while(|c| *c != '"').collect();
                tags.insert(tag);
            }
        }
        tags
    }

    /// Whether `text` names `Intent::{name}` as a whole word.
    fn names_intent(text: &str, name: &str) -> bool {
        let needle = format!("Intent::{name}");
        text.match_indices(&needle)
            .any(|(at, _)| !text[at + needle.len()..].starts_with(|c: char| c.is_alphanumeric()))
    }

    /// The phone's built cells that the session handles (`Intent::X` in
    /// `session.rs`) are reached from the app too (1496): a request the
    /// phone's decode turns into that intent, and a Java builder that sends
    /// its tag. The Viewport cell said built for a week because the guard
    /// read only `session.rs`; no Java ever sent a viewport, so the phone
    /// sent no read receipt to anyone.
    #[test]
    fn every_phone_cell_the_session_handles_is_sent_by_the_java() {
        let (arms, tags) = (decoded_requests(), java_tags());
        assert!(arms.len() > 40, "the decode's arms: {}", arms.len());
        assert!(tags.contains("open"), "the Java's tags: {tags:?}");
        let mut wrong = Vec::new();
        for row in INVENTORY {
            let Cell::Built(Via::At(SESSION, token)) = row.phone else {
                continue;
            };
            let Some(name) = token.strip_prefix("Intent::") else {
                continue;
            };
            let raised: Vec<&String> = arms
                .iter()
                .filter(|(_, text)| names_intent(text, name))
                .map(|(tag, _)| tag)
                .collect();
            if raised.is_empty() {
                wrong.push(format!("{}: no request decodes to it", row.intent));
            } else if !raised.iter().any(|tag| tags.contains(*tag)) {
                wrong.push(format!("{}: no Java sends {raised:?}", row.intent));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The control for the guard above: a tag no Java sends is caught, and
    /// the viewport's builder is seen.
    #[test]
    fn a_phone_cell_without_its_java_is_caught() {
        let tags = java_tags();
        assert!(tags.contains("viewport"), "Core.viewport sends it");
        assert!(!tags.contains("nosuchrequest"));
        let arms = decoded_requests();
        let viewport: Vec<&String> = arms
            .iter()
            .filter(|(_, text)| names_intent(text, "Viewport"))
            .map(|(tag, _)| tag)
            .collect();
        assert_eq!(viewport, ["viewport"]);
        assert!(!names_intent("Intent::OpenChat", "Open"));
    }

    /// The control for the guard above: a cell that claims a key no chord
    /// has, and a token its file lacks, are both caught.
    #[test]
    fn a_false_cell_is_caught() {
        assert!(
            !keyed().contains("Network"),
            "control: a platform fact has no chord"
        );
        assert!(keyed().contains("Profile"), "P opens the profile");
        let text = std::fs::read_to_string(root().join(GUI)).expect("gui.rs");
        assert!(!text.contains("Intent::Import"), "the window has no import");
    }

    /// The page's section is this table, byte for byte, between its
    /// markers; `LOQUAX_WRITE_INVENTORY=1` writes it there.
    #[test]
    fn the_page_shows_the_inventory() {
        const BEGIN: &str = "<!-- intent-inventory:begin (loquax/src/ui/inventory.rs) -->\n";
        const END: &str = "<!-- intent-inventory:end -->";
        let path = root().join("docs/src/concepts/loquax-ui.md");
        let page = std::fs::read_to_string(&path).expect("loquax-ui.md");
        let start = page.find(BEGIN).expect("the begin marker") + BEGIN.len();
        let end = page.find(END).expect("the end marker");
        let table = markdown();
        if std::env::var_os("LOQUAX_WRITE_INVENTORY").is_some() {
            let written = format!("{}{}{}", &page[..start], table, &page[end..]);
            std::fs::write(&path, written).expect("write the page");
            return;
        }
        assert!(
            page[start..end] == table,
            "the page's inventory differs from loquax/src/ui/inventory.rs; \
             LOQUAX_WRITE_INVENTORY=1 writes it"
        );
    }
}
