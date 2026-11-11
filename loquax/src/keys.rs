//! loquax's keymap: its rows beside the shared scroll rows, and the four
//! readers of them (`docs/src/concepts/loquax-keys-and-layout.md`, sections 3
//! and 4).
//!
//! [`resolve`] is the key handler's lookup, [`help_groups`] the help
//! overlay, [`footer`] the footer strip and [`compose_hint`] the line on the
//! compose pane's border; the palette's names are rows too
//! ([`leviculum_ui::keymap::Row::palette`]). The tables are data, the per-pane
//! rules of section 3 ("How the collisions are settled") are each chord's
//! panes, and the tests at the end hold what the page asks of the table: no
//! chord reaches two actions in one pane, every row is in the help and every
//! palette name has a row, the quiet keyboard, a plain chord beside every
//! enhanced one, and `:` `q` `Enter` quits.
//!
//! The search (slice 4) adds a pane and a context: [`SEARCH`] is its field,
//! and while one is open ([`SEARCHING`]) the list is its results, `Enter`
//! there opens a result, `Esc` closes the search, and `n`/`N` and
//! `Ctrl-s`/`Ctrl-r` step the matches, which outside a search is not their
//! meaning (`Ctrl-r` fetches then).
//!
//! Delete (`d`, then `y`) and the palette's `delete conversation` and `pin`
//! came with the history (slice 6); `rename`, `block`, `unblock` and
//! `blocked` with the contacts (1082), palette names without a chord.
//!
//! The map (`docs/src/concepts/loquax-map.md`, slice (c)) adds a pane,
//! [`MAP`], in the conversation's place: `M` from the two command panes and
//! `Alt-m` from every pane open it, as does the palette's `map`. In it the
//! arrows and vi's `h` `j` `k` `l` pan, `+` and `-` zoom, `0` fits every
//! pin again, `Tab` steps the pins, `Enter` opens the selected peer's
//! conversation and `Esc` or `Ctrl-g` closes it. `m` stays free for a new
//! conversation (section 3). `t` and `T` (and `F2`) cycle the ways the map
//! is drawn, which the palette's `map drawing` lists, and `g` puts the glyph
//! test over it (1093).
//!
//! `Enter` on a message opens it (1076): a position report opens the map at
//! its fix, any other message its detail view, which `I` opens for every
//! message (`v` until 1475, which made `v` the copy mode,
//! [`crate::ui::copy_mode`]). `L` in the two command panes and `Alt-l` from every pane put the
//! open conversation's peer on the map.
//!
//! A voice message (1276, `docs/src/concepts/loquax-voice-recording.md`):
//! `Alt-v` in compose records, locked at once, since a terminal has no
//! hold. Plain `v` cannot start it: in compose `v` is a letter, and a
//! message that begins with one ("vielleicht") would start a recording.
//! While the recording bar stands in the field's place ([`RECORDING`])
//! nothing is typed, so the bar's keys are plain: `v` (or `Alt-v`, or the
//! send chord) sends, `Esc` and `Ctrl-g` discard, `p` pauses and resumes,
//! `Space` plays what is paused.
//!
//! Not here yet, because what they run is a later slice: new conversation
//! (`m`, `C`), hints (`f`),
//! and `Ctrl-x Ctrl-c` (a two-key sequence, which the table does not
//! model). The mouse is not a chord: what it does is [`crate::ui::hit`],
//! and only its capture switch is a row, the palette's `mouse`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::commands::Command;
use leviculum_ui::footer::FooterItem;
use leviculum_ui::help::{HelpEntry, HelpGroup};
use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::keymap::{self, Chord, Ctx, Mods, Panes, Row, ScrollCmd, MOVE, SCROLL_ROWS};

/// The conversation list.
pub const LIST: Panes = Panes(1 << 1);
/// The messages.
pub const MESSAGES: Panes = Panes(1 << 2);
/// The compose pane.
pub const COMPOSE: Panes = Panes(1 << 3);
/// An open overlay (the help, the palette): only what is bound here reaches
/// past it.
pub const OVERLAY: Panes = Panes(1 << 4);
/// The search field, the design page's **S**: it takes plain typing, as
/// compose does.
pub const SEARCH: Panes = Panes(1 << 5);
/// The map, in the conversation pane's place.
pub const MAP: Panes = Panes(1 << 6);
/// The two command panes.
pub const COMMAND_PANES: Panes = Panes(LIST.0 | MESSAGES.0);
/// The three panes `Tab` cycles through outside a search.
pub const THREE_PANES: Panes = Panes(LIST.0 | MESSAGES.0 | COMPOSE.0);
/// Every pane outside an overlay, the design page's **all**.
pub const EVERY: Panes = Panes(THREE_PANES.0 | SEARCH.0 | MAP.0);
/// Where the results of an open search are stepped: the field, and the list
/// which shows them.
const RESULTS: Panes = Panes(SEARCH.0 | LIST.0);

/// `send_on_enter` is off: `Enter` breaks the line in compose and the
/// modifier chord sends.
pub const ENTER_BREAKS: Ctx = Ctx(1 << 1);
/// A search is open: the list shows its results.
pub const SEARCHING: Ctx = Ctx(1 << 2);
/// A voice message records for the open conversation (1276): the
/// recording bar is the compose box, and its keys are the bar's.
pub const RECORDING: Ctx = Ctx(1 << 3);

/// What a key runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A palette command: the same thing the palette runs under its name.
    Run(Command),
    /// Move the focused list's cursor.
    Scroll(ScrollCmd),
    /// Move the messages' cursor from the compose pane.
    ScrollMessages(ScrollCmd),
    /// Break the compose line.
    Newline,
    /// Hand the key to the compose buffer's editing table.
    Edit,
    /// Clear compose into the kill buffer, back with `Ctrl-y`.
    ClearDraft,
    /// Back out: compose to messages to the list, and an overlay closes.
    Back,
    /// The next pane.
    NextPane,
    /// The previous pane.
    PreviousPane,
    /// Open the command palette.
    Palette,
    /// `Ctrl-c` outside compose: say what quits.
    QuitHint,
    /// Copy the drag's selection, or the message under the cursor.
    Copy,
    /// The copy mode on (1475): a cursor on the text that selects as it
    /// moves.
    CopyMode,
    /// Open the selected result of the search.
    OpenResult,
    /// The next match (`true`) or the previous one: a result row in the
    /// field and the list, a message in the open conversation.
    Match(bool),
    /// Close the search; the list comes back.
    CloseSearch,
    /// Something the map does.
    Map(MapCmd),
    /// Open the message under the cursor: a position report the map at its
    /// fix, any other message its detail view.
    OpenMessage,
    /// The open conversation's peer on the map, at its latest fix.
    PeerOnMap,
    /// Something the recording of a voice message does (1276).
    Record(RecKey),
}

/// What a key does to a voice message's recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecKey {
    /// Start one, locked.
    Start,
    /// Send it.
    Send,
    /// Throw it away.
    Discard,
    /// Pause it, or go on with it.
    Pause,
    /// Hear what is paused, or stop hearing it.
    Play,
}

/// What a key does on the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapCmd {
    ZoomIn,
    ZoomOut,
    /// Move the view by eighths of its size: west, east, north, south.
    Pan(i8, i8),
    /// Every pin in view again, as on open.
    Fit,
    /// The next pin in `Tab` order (`true`) or the previous one.
    Step(bool),
    /// The selected pin's conversation.
    Open,
    /// The conversation comes back.
    Close,
    /// The next way of drawing the map (`true`) or the previous one.
    Drawing(bool),
    /// The glyph test pattern over the map, or away again.
    GlyphTest,
    /// The tracks' next time window (`true`) or the previous one (1385).
    TrackWindow(bool),
    /// Every peer with a position inside the tracks' time window in view
    /// (1386).
    FitWindow,
    /// The time zone view on or off (1684): the zones' borders and each
    /// one's clock over the map.
    TimeZones,
}

const fn key(code: KeyCode, mods: Mods, action: Action, panes: Panes) -> Chord<Action> {
    Chord::new(code, mods, action).panes(panes)
}

const NONE: Mods = Mods::Exact(KeyModifiers::NONE);
const CTRL: Mods = Mods::Exact(KeyModifiers::CONTROL);
const ALT: Mods = Mods::Exact(KeyModifiers::ALT);
const SHIFT: Mods = Mods::Exact(KeyModifiers::SHIFT);
const ALT_SHIFT: Mods = Mods::Exact(KeyModifiers::from_bits_truncate(
    KeyModifiers::ALT.bits() | KeyModifiers::SHIFT.bits(),
));
const CTRL_SHIFT: Mods = Mods::Exact(KeyModifiers::from_bits_truncate(
    KeyModifiers::CONTROL.bits() | KeyModifiers::SHIFT.bits(),
));

const fn run(command: Command) -> Action {
    Action::Run(command)
}

/// The help group titles, in the overlay's order. The first is the shared
/// rows' group.
pub const GROUPS: [&str; 6] = [
    MOVE,
    CONVERSATIONS,
    MESSAGES_GROUP,
    COMPOSE_GROUP,
    MAP_GROUP,
    EVERYWHERE,
];
const CONVERSATIONS: &str = "Conversations";
const MAP_GROUP: &str = "Map";
const MESSAGES_GROUP: &str = "Messages";
const COMPOSE_GROUP: &str = "Compose";
const EVERYWHERE: &str = "Everywhere";

/// loquax's own rows, beside [`SCROLL_ROWS`], which apply in the two command
/// panes.
pub const ROWS: &[Row<Action>] = {
    use Action::*;
    use Command as C;
    use KeyCode::*;
    use ScrollCmd::{Bottom, PageDown as PgDn, PageUp as PgUp, Top};
    &[
        // Move: the one chord loquax adds to the shared rows, and paging the
        // messages while composing.
        Row::new(
            "top / bottom (Signal)",
            MOVE,
            &[&[
                key(Up, CTRL, Scroll(Top), COMMAND_PANES),
                key(Down, CTRL, Scroll(Bottom), COMMAND_PANES),
            ]],
        ),
        Row::new(
            "page the messages (compose)",
            MOVE,
            &[&[
                key(PageDown, Mods::Any, ScrollMessages(PgDn), COMPOSE),
                key(PageUp, Mods::Any, ScrollMessages(PgUp), COMPOSE),
            ]],
        ),
        // The chat is read by the line (1529): Up and Down a line, Left
        // and Right a screen less a line, the ring's four on the M9.
        Row::new(
            "page the chat (left / right)",
            MOVE,
            &[&[
                key(Left, NONE, Scroll(PgUp), MESSAGES),
                key(Right, NONE, Scroll(PgDn), MESSAGES),
            ]],
        ),
        // Conversations.
        Row::new(
            "open the selected conversation",
            CONVERSATIONS,
            &[&[key(Enter, NONE, run(C::FocusMessages), LIST).unless(SEARCHING)]],
        )
        .palette("focus messages")
        .footer("open"),
        Row::new(
            "open the selected result",
            CONVERSATIONS,
            &[&[key(Enter, NONE, OpenResult, RESULTS).when(SEARCHING)]],
        )
        .footer("open"),
        // Messages; listed here so its footer leads the messages pane's.
        Row::new(
            "open the message: a position on the map, any other its details",
            MESSAGES_GROUP,
            &[&[key(Enter, NONE, OpenMessage, MESSAGES)]],
        )
        .footer("open"),
        Row::new(
            "the message's details",
            MESSAGES_GROUP,
            &[&[key(Char('I'), Mods::Plain, run(C::Details), MESSAGES)]],
        )
        .palette("details")
        .note("Ctrl-D in Signal, Info in WhatsApp: here I (v is the copy mode)"),
        // The copy mode (1475): vi's visual `v`, the Emacs mark, and a
        // Shift+arrow, which moves the new cursor at once.
        Row::new(
            "select text with a cursor: the copy mode",
            MESSAGES_GROUP,
            &[
                &[key(Char('v'), Mods::Plain, CopyMode, MESSAGES)],
                &[key(Char(' '), CTRL, CopyMode, MESSAGES)],
                // Quiet: the help's key column would grow for all rows;
                // the keys page names them.
                &[
                    key(Left, SHIFT, CopyMode, MESSAGES).quiet(),
                    key(Right, SHIFT, CopyMode, MESSAGES).quiet(),
                    key(Up, SHIFT, CopyMode, MESSAGES).quiet(),
                    key(Down, SHIFT, CopyMode, MESSAGES).quiet(),
                ],
            ],
        )
        .note("then move, y or Alt-w copies, Esc leaves"),
        Row::new(
            "copy the message's text, or what a drag selected",
            MESSAGES_GROUP,
            &[
                &[key(Char('y'), Mods::Plain, Copy, MESSAGES)],
                &[key(Char('w'), ALT, Copy, MESSAGES)],
            ],
        ),
        Row::new(
            "delete the message",
            MESSAGES_GROUP,
            &[&[key(Char('d'), Mods::Plain, run(C::DeleteMessage), MESSAGES)]],
        )
        .palette("delete message")
        .note("on this device; y confirms"),
        Row::new(
            "react to the message",
            MESSAGES_GROUP,
            &[&[key(Char('e'), Mods::Plain, run(C::React), MESSAGES)]],
        )
        .palette("react")
        .note("1-6 a quick emoji, or type one"),
        Row::new("delete the conversation", CONVERSATIONS, &[])
            .palette("delete conversation")
            .note("on this device; y confirms"),
        Row::new("pin or unpin the conversation", CONVERSATIONS, &[]).palette("pin"),
        Row::new("name the peer", CONVERSATIONS, &[])
            .palette("rename")
            .note("empty clears"),
        Row::new("block the peer", CONVERSATIONS, &[])
            .palette("block")
            .note("kept, not shown"),
        Row::new("unblock the peer", CONVERSATIONS, &[]).palette("unblock"),
        Row::new("list the blocked, or the rest", CONVERSATIONS, &[]).palette("blocked"),
        Row::new("archive, or bring back", CONVERSATIONS, &[])
            .palette("archive")
            .note("a message brings it back unless muted"),
        Row::new("list the archived, or the rest", CONVERSATIONS, &[]).palette("archived"),
        Row::new("start a conversation", CONVERSATIONS, &[])
            .palette("new conversation")
            .note("a contact's code or address"),
        Row::new("add a contact", CONVERSATIONS, &[])
            .palette("add contact")
            .note("a contact's code, then a name"),
        // The profile (1443): what a tap on the avatar opens on the phone
        // and a click on it in the window; here `P`, and a click on the
        // list's title, which is the reader's own name.
        Row::new(
            "your profile: name, address, code and QR",
            CONVERSATIONS,
            &[&[key(Char('P'), Mods::Plain, run(C::MyCode), COMMAND_PANES)]],
        )
        .palette("my code")
        .note("for a contact to scan or paste"),
        // The settings (1450): the phone's ⋮ menu and the window's gear;
        // here `S`, beside the profile's `P`.
        Row::new(
            "settings: profile, mailbox, appearance, chats, privacy and more",
            CONVERSATIONS,
            &[&[key(Char('S'), Mods::Plain, run(C::Settings), COMMAND_PANES)]],
        )
        .palette("settings"),
        Row::new("import from another messenger", CONVERSATIONS, &[])
            .palette("import")
            .note("contacts and history; the identity: loquax import"),
        Row::new(
            "previous conversation",
            CONVERSATIONS,
            &[
                &[key(Up, ALT, run(C::PreviousConversation), EVERY)],
                &[key(BackTab, CTRL_SHIFT, run(C::PreviousConversation), EVERY).enhanced()],
            ],
        )
        .palette("previous conversation"),
        Row::new(
            "next conversation",
            CONVERSATIONS,
            &[
                &[key(Down, ALT, run(C::NextConversation), EVERY)],
                &[key(Tab, CTRL, run(C::NextConversation), EVERY).enhanced()],
            ],
        )
        .palette("next conversation"),
        Row::new(
            "previous unread",
            CONVERSATIONS,
            &[&[key(Up, ALT_SHIFT, run(C::PreviousUnread), EVERY)]],
        )
        .palette("previous unread"),
        Row::new(
            "next unread",
            CONVERSATIONS,
            &[&[key(Down, ALT_SHIFT, run(C::NextUnread), EVERY)]],
        )
        .palette("next unread")
        .note("Tab in mutt: here Alt-Shift-↓, Tab cycles the panes"),
        Row::new(
            "write in this conversation",
            CONVERSATIONS,
            &[
                &[
                    key(Char('i'), Mods::Plain, run(C::FocusCompose), COMMAND_PANES),
                    key(Char('a'), Mods::Plain, run(C::FocusCompose), COMMAND_PANES),
                ],
                &[key(
                    Char('r'),
                    Mods::Plain,
                    run(C::FocusCompose),
                    COMMAND_PANES,
                )],
                &[key(Char('T'), CTRL_SHIFT, run(C::FocusCompose), COMMAND_PANES).enhanced()],
            ],
        )
        .palette("focus compose")
        .footer_droppable("write", 3),
        Row::new("focus the conversation list", CONVERSATIONS, &[]).palette("focus conversations"),
        // Search (slice 4): `Ctrl-s` opens it and, once it is open, steps
        // to the next match, as Emacs's incremental search does; in compose
        // it always goes back to the field.
        Row::new(
            "search names and messages",
            CONVERSATIONS,
            &[
                &[key(Char('/'), Mods::Plain, run(C::Search), COMMAND_PANES)],
                &[
                    key(Char('s'), CTRL, run(C::Search), COMMAND_PANES).unless(SEARCHING),
                    key(Char('s'), CTRL, run(C::Search), COMPOSE).quiet(),
                ],
                &[key(Char('F'), CTRL_SHIFT, run(C::Search), EVERY).enhanced()],
            ],
        )
        .palette("search")
        .note("Ctrl-F in Signal: here /"),
        Row::new(
            "next / previous match",
            CONVERSATIONS,
            &[
                &[
                    key(Char('n'), Mods::Plain, Match(true), COMMAND_PANES).when(SEARCHING),
                    key(Char('N'), Mods::Plain, Match(false), COMMAND_PANES).when(SEARCHING),
                ],
                &[
                    key(Char('s'), CTRL, Match(true), RESULTS.with(MESSAGES)).when(SEARCHING),
                    key(Char('r'), CTRL, Match(false), RESULTS.with(MESSAGES)).when(SEARCHING),
                ],
            ],
        )
        .footer_spelled("next, previous"),
        Row::new(
            "search field to its results",
            CONVERSATIONS,
            &[&[
                key(Tab, Mods::Any, NextPane, SEARCH),
                key(BackTab, Mods::Any, PreviousPane, SEARCH),
            ]],
        )
        .footer_droppable("to the results", 4),
        Row::new(
            "close the search",
            CONVERSATIONS,
            &[
                &[key(Esc, NONE, CloseSearch, RESULTS).when(SEARCHING)],
                &[key(Char('g'), CTRL, CloseSearch, RESULTS).when(SEARCHING)],
            ],
        )
        .footer("close search"),
        // Compose. The Enter rule: Enter sends and Shift-Enter (enhanced),
        // Alt-Enter and Ctrl-j break the line; Ctrl-Enter (enhanced) always
        // sends and never breaks. With send_on_enter off, Enter breaks and
        // Alt-Enter sends.
        Row::new(
            "send",
            COMPOSE_GROUP,
            &[
                &[key(Enter, NONE, run(C::Send), COMPOSE).unless(ENTER_BREAKS)],
                &[key(Enter, CTRL, run(C::Send), COMPOSE).enhanced()],
                &[key(Enter, ALT, run(C::Send), COMPOSE).when(ENTER_BREAKS)],
            ],
        )
        .palette("send")
        .footer("send")
        .note("with send on enter off, Enter breaks the line and the modifier chord sends")
        .tx(),
        Row::new(
            "line break",
            COMPOSE_GROUP,
            &[
                &[key(Enter, NONE, Newline, COMPOSE).when(ENTER_BREAKS)],
                &[key(Enter, SHIFT, Newline, COMPOSE).enhanced()],
                &[key(Enter, ALT, Newline, COMPOSE).unless(ENTER_BREAKS)],
                &[key(Char('j'), CTRL, Newline, COMPOSE)],
            ],
        ),
        Row::new(
            "line start / end",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('a'), CTRL, Edit, COMPOSE),
                    key(Char('e'), CTRL, Edit, COMPOSE),
                ],
                &[
                    key(Home, NONE, Edit, COMPOSE),
                    key(End, NONE, Edit, COMPOSE),
                ],
            ],
        ),
        Row::new(
            "line up / down",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('p'), CTRL, Edit, COMPOSE),
                    key(Char('n'), CTRL, Edit, COMPOSE),
                ],
                &[key(Up, NONE, Edit, COMPOSE), key(Down, NONE, Edit, COMPOSE)],
            ],
        ),
        Row::new(
            "body start / end",
            COMPOSE_GROUP,
            &[&[
                key(Char('<'), Mods::Alt, Edit, COMPOSE),
                key(Char('>'), Mods::Alt, Edit, COMPOSE),
            ]],
        ),
        Row::new(
            "character back / forward",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('b'), CTRL, Edit, COMPOSE),
                    key(Char('f'), CTRL, Edit, COMPOSE),
                ],
                &[
                    key(Left, NONE, Edit, COMPOSE),
                    key(Right, NONE, Edit, COMPOSE),
                ],
            ],
        ),
        Row::new(
            "word back / forward",
            COMPOSE_GROUP,
            &[
                &[
                    key(Char('b'), Mods::Alt, Edit, COMPOSE),
                    key(Char('f'), Mods::Alt, Edit, COMPOSE),
                ],
                &[
                    key(Left, CTRL, Edit, COMPOSE),
                    key(Right, CTRL, Edit, COMPOSE),
                ],
            ],
        ),
        Row::new(
            "delete forward",
            COMPOSE_GROUP,
            &[
                &[key(Char('d'), CTRL, Edit, COMPOSE)],
                &[key(Delete, NONE, Edit, COMPOSE)],
            ],
        ),
        Row::new(
            "delete a word back / forward",
            COMPOSE_GROUP,
            &[
                &[
                    key(Backspace, Mods::Alt, Edit, COMPOSE),
                    key(Char('d'), Mods::Alt, Edit, COMPOSE),
                ],
                &[key(Char('w'), CTRL, Edit, COMPOSE)],
            ],
        ),
        Row::new(
            "kill to the end, yank",
            COMPOSE_GROUP,
            &[&[
                key(Char('k'), CTRL, Edit, COMPOSE),
                key(Char('y'), CTRL, Edit, COMPOSE),
            ]],
        ),
        Row::new(
            "clear to a draft Ctrl-y brings back",
            COMPOSE_GROUP,
            &[&[key(Char('c'), CTRL, ClearDraft, COMPOSE)]],
        ),
        // A voice message (1276): the bar's keys while it records.
        Row::new(
            "record a voice message",
            COMPOSE_GROUP,
            &[&[key(Char('v'), ALT, Record(RecKey::Start), COMPOSE).unless(RECORDING)]],
        )
        .note("starts locked; v alone is a letter while you type"),
        Row::new(
            "send the voice message",
            COMPOSE_GROUP,
            &[&[
                key(Char('v'), Mods::Plain, Record(RecKey::Send), COMPOSE).when(RECORDING),
                key(Char('v'), ALT, Record(RecKey::Send), COMPOSE)
                    .when(RECORDING)
                    .quiet(),
            ]],
        )
        .note("while recording; the send chord sends it too")
        .tx(),
        Row::new(
            "throw the voice message away",
            COMPOSE_GROUP,
            &[
                &[key(Esc, NONE, Record(RecKey::Discard), COMPOSE).when(RECORDING)],
                &[key(Char('g'), CTRL, Record(RecKey::Discard), COMPOSE).when(RECORDING)],
            ],
        ),
        Row::new(
            "pause or go on recording",
            COMPOSE_GROUP,
            &[&[key(Char('p'), Mods::Plain, Record(RecKey::Pause), COMPOSE).when(RECORDING)]],
        ),
        Row::new(
            "listen to the paused recording, or stop",
            COMPOSE_GROUP,
            &[&[key(Char(' '), Mods::Plain, Record(RecKey::Play), COMPOSE).when(RECORDING)]],
        ),
        // The map.
        Row::new(
            "the map of the peers' positions",
            MAP_GROUP,
            &[
                &[key(Char('M'), Mods::Plain, run(C::Map), COMMAND_PANES)],
                &[key(Char('m'), ALT, run(C::Map), EVERY)],
            ],
        )
        .palette("map"),
        // The Browse tab (1369): NomadNet pages, lnomad's own screen.
        Row::new(
            "the NomadNet pages (the Browse tab)",
            MAP_GROUP,
            &[&[key(Char('B'), Mods::Plain, run(C::Browse), COMMAND_PANES)]],
        )
        .palette("browse"),
        // The Rooms tab (1379): Reticulum Relay Chat on the config's hub.
        // In the tab its input line has the keys (`crate::ui::rooms::key`).
        Row::new(
            "the live rooms on a hub (the Rooms tab)",
            MAP_GROUP,
            &[&[key(Char('R'), Mods::Plain, run(C::Rooms), COMMAND_PANES)]],
        )
        .palette("rooms"),
        // The Explore tab (1578): the pages and the public rooms under
        // one button. In the tab its search field and its cursor have the
        // keys (`crate::ui::explore::key`).
        Row::new(
            "the pages and the public rooms (the Explore tab)",
            MAP_GROUP,
            &[&[key(Char('E'), Mods::Plain, run(C::Explore), COMMAND_PANES)]],
        )
        .palette("explore"),
        Row::new(
            "this conversation's peer on the map",
            MAP_GROUP,
            &[
                &[
                    key(Char('L'), Mods::Plain, PeerOnMap, COMMAND_PANES),
                    // The entry's map pin (1224).
                    key(Char('m'), Mods::Plain, PeerOnMap, LIST),
                ],
                &[key(Char('l'), ALT, PeerOnMap, EVERY)],
            ],
        ),
        Row::new(
            "zoom in / out",
            MAP_GROUP,
            &[
                &[
                    key(Char('+'), Mods::Plain, Map(MapCmd::ZoomIn), MAP),
                    key(Char('-'), Mods::Plain, Map(MapCmd::ZoomOut), MAP),
                ],
                &[key(Char('='), Mods::Plain, Map(MapCmd::ZoomIn), MAP).quiet()],
            ],
        )
        .footer_droppable("zoom", 3),
        Row::new(
            "pan",
            MAP_GROUP,
            &[
                &[
                    key(Left, NONE, Map(MapCmd::Pan(-1, 0)), MAP),
                    key(Down, NONE, Map(MapCmd::Pan(0, 1)), MAP),
                    key(Up, NONE, Map(MapCmd::Pan(0, -1)), MAP),
                    key(Right, NONE, Map(MapCmd::Pan(1, 0)), MAP),
                ],
                &[
                    key(Char('h'), Mods::Plain, Map(MapCmd::Pan(-1, 0)), MAP),
                    key(Char('j'), Mods::Plain, Map(MapCmd::Pan(0, 1)), MAP),
                    key(Char('k'), Mods::Plain, Map(MapCmd::Pan(0, -1)), MAP),
                    key(Char('l'), Mods::Plain, Map(MapCmd::Pan(1, 0)), MAP),
                ],
            ],
        ),
        // A key moves an eighth, so one press is a small step; Shift a half
        // (1075). The Shift arrows resolve but are named in the text: four
        // `Shift-` chords would widen the help's key column for every row.
        Row::new(
            "pan by half the view, also Shift and an arrow",
            MAP_GROUP,
            &[
                &[
                    key(Char('H'), Mods::Plain, Map(MapCmd::Pan(-4, 0)), MAP),
                    key(Char('J'), Mods::Plain, Map(MapCmd::Pan(0, 4)), MAP),
                    key(Char('K'), Mods::Plain, Map(MapCmd::Pan(0, -4)), MAP),
                    key(Char('L'), Mods::Plain, Map(MapCmd::Pan(4, 0)), MAP),
                ],
                &[
                    key(Left, SHIFT, Map(MapCmd::Pan(-4, 0)), MAP).quiet(),
                    key(Down, SHIFT, Map(MapCmd::Pan(0, 4)), MAP).quiet(),
                    key(Up, SHIFT, Map(MapCmd::Pan(0, -4)), MAP).quiet(),
                    key(Right, SHIFT, Map(MapCmd::Pan(4, 0)), MAP).quiet(),
                ],
            ],
        ),
        Row::new(
            "load the map of the visible area",
            MAP_GROUP,
            &[&[key(Char('d'), Mods::Plain, run(C::LoadMapArea), MAP)]],
        )
        .palette("load map area"),
        // 1093: the drawing cycles live, every way the shell has, the one
        // the reader stays on remembered; the glyph test says which glyph
        // sets the font draws.
        Row::new(
            "next / previous way of drawing the map",
            MAP_GROUP,
            &[
                &[
                    key(Char('t'), Mods::Plain, Map(MapCmd::Drawing(true)), MAP),
                    key(Char('T'), Mods::Plain, Map(MapCmd::Drawing(false)), MAP),
                ],
                &[key(F(2), Mods::Any, Map(MapCmd::Drawing(true)), MAP)],
            ],
        ),
        Row::new(
            "every way of drawing the map, the one now marked",
            MAP_GROUP,
            &[],
        )
        .palette("map drawing"),
        // 1385: the tracks' time window, a map control: now (the current
        // episode), today, yesterday, this week, this month, all.
        Row::new(
            "next / previous time window of the tracks",
            MAP_GROUP,
            &[&[
                key(Char('w'), Mods::Plain, Map(MapCmd::TrackWindow(true)), MAP),
                key(Char('W'), Mods::Plain, Map(MapCmd::TrackWindow(false)), MAP),
            ]],
        ),
        // 1684: the time zone view, the window's and the phone's map menu
        // entry.
        Row::new(
            "the time zones and their clocks over the map, or away again",
            MAP_GROUP,
            &[&[key(Char('z'), Mods::Plain, Map(MapCmd::TimeZones), MAP)]],
        ),
        Row::new(
            "the glyph test: which glyph sets the font draws",
            MAP_GROUP,
            &[&[key(Char('g'), Mods::Plain, Map(MapCmd::GlyphTest), MAP)]],
        ),
        Row::new(
            "every pin in view again",
            MAP_GROUP,
            &[&[key(Char('0'), Mods::Plain, Map(MapCmd::Fit), MAP)]],
        )
        .footer_droppable("fit", 4),
        // 1386: "fit all", every peer with a position inside the tracks'
        // time window; the window's strip and the phone's chips have it
        // as a button.
        Row::new(
            "every peer inside the time window in view",
            MAP_GROUP,
            &[&[key(Char('f'), Mods::Plain, Map(MapCmd::FitWindow), MAP)]],
        ),
        Row::new(
            "next / previous pin",
            MAP_GROUP,
            &[&[
                key(Tab, NONE, Map(MapCmd::Step(true)), MAP),
                key(BackTab, Mods::Any, Map(MapCmd::Step(false)), MAP),
            ]],
        )
        .footer("pin"),
        Row::new(
            "open the pin's conversation",
            MAP_GROUP,
            &[&[key(Enter, NONE, Map(MapCmd::Open), MAP)]],
        )
        .footer("open"),
        Row::new(
            "close the map",
            MAP_GROUP,
            &[
                &[key(Esc, NONE, Map(MapCmd::Close), MAP)],
                &[key(Char('g'), CTRL, Map(MapCmd::Close), MAP)],
            ],
        )
        .footer("close"),
        // Everywhere.
        Row::new(
            "next / previous pane",
            EVERYWHERE,
            &[&[
                key(Tab, Mods::Any, NextPane, THREE_PANES),
                key(BackTab, Mods::Any, PreviousPane, THREE_PANES),
            ]],
        )
        .footer("pane"),
        Row::new(
            "back out: compose, messages, list; close an overlay",
            EVERYWHERE,
            // In compose while a voice message records, the two throw it
            // away instead (1276).
            &[
                &[
                    key(Esc, NONE, Back, Panes(MESSAGES.0 | OVERLAY.0)),
                    key(Esc, NONE, Back, COMPOSE).unless(RECORDING).quiet(),
                ],
                &[
                    key(Char('g'), CTRL, Back, Panes(MESSAGES.0 | OVERLAY.0)),
                    key(Char('g'), CTRL, Back, COMPOSE)
                        .unless(RECORDING)
                        .quiet(),
                ],
            ],
        )
        .footer_droppable("back", 5),
        Row::new(
            "command palette",
            EVERYWHERE,
            &[
                &[key(
                    Char(':'),
                    Mods::Plain,
                    Palette,
                    COMMAND_PANES.with(MAP),
                )],
                &[key(Char('x'), ALT, Palette, EVERY)],
            ],
        )
        .footer_droppable("commands", 2)
        .note("Ctrl-P moves a line in every pane: the palette is : or Alt-x"),
        Row::new(
            "help",
            EVERYWHERE,
            &[
                &[key(
                    Char('?'),
                    Mods::Plain,
                    run(C::Help),
                    COMMAND_PANES.with(MAP),
                )],
                &[key(F(1), Mods::Any, run(C::Help), EVERY)],
            ],
        )
        .palette("help")
        .footer_droppable("help", 1),
        Row::new(
            "check the propagation node",
            EVERYWHERE,
            &[
                &[key(F(5), Mods::Any, run(C::Fetch), EVERY)],
                &[key(Char('r'), CTRL, run(C::Fetch), COMMAND_PANES).unless(SEARCHING)],
            ],
        )
        .palette("fetch")
        .tx(),
        Row::new(
            "redraw",
            EVERYWHERE,
            &[&[key(Char('l'), CTRL, run(C::Redraw), EVERY)]],
        )
        .palette("redraw"),
        Row::new(
            "quit",
            EVERYWHERE,
            &[
                &[key(Char('q'), Mods::Plain, run(C::Quit), COMMAND_PANES)],
                &[key(
                    Char('q'),
                    CTRL,
                    run(C::Quit),
                    Panes(EVERY.0 | OVERLAY.0),
                )],
            ],
        )
        .palette("quit")
        .footer("quit"),
        Row::new(
            "says what quits (Ctrl-c quits nowhere)",
            EVERYWHERE,
            &[&[key(
                Char('c'),
                CTRL,
                QuitHint,
                Panes(LIST.0 | MESSAGES.0 | SEARCH.0 | MAP.0 | OVERLAY.0),
            )]],
        ),
        Row::new("Enter sends, or breaks the line", EVERYWHERE, &[]).palette("send on enter"),
        Row::new(
            "mouse capture off and on (--no-mouse starts it off)",
            EVERYWHERE,
            &[],
        )
        .palette("mouse"),
        Row::new(
            "light and dark colours swapped for the session (--theme, the theme key)",
            EVERYWHERE,
            &[],
        )
        .palette("theme"),
        Row::new(
            "read receipts off and on (the read_receipts key)",
            EVERYWHERE,
            &[],
        )
        .palette("read receipts")
        .note("off, none is sent and none is shown"),
        Row::new(
            "make a group: tick contacts by name, then its name (1307)",
            EVERYWHERE,
            &[],
        )
        .palette("new group"),
        Row::new(
            "join the group the open conversation's invitation names",
            EVERYWHERE,
            &[],
        )
        .palette("join group"),
        Row::new("the open group's members", EVERYWHERE, &[]).palette("members"),
        Row::new("leave the open group, after asking", EVERYWHERE, &[]).palette("leave group"),
        Row::new(
            "remove a member from the open group, its admin's",
            EVERYWHERE,
            &[],
        )
        .palette("remove member"),
        Row::new(
            "add members to the open group, its admin's: tick contacts, then Enter",
            EVERYWHERE,
            &[],
        )
        .palette("add members"),
        Row::new(
            "delete the open group for everyone, its admin's, after asking",
            EVERYWHERE,
            &[],
        )
        .palette("delete group"),
        Row::new(
            "make a left or ended group again with the same people",
            EVERYWHERE,
            &[],
        )
        .palette("start again"),
        Row::new(
            "answer the message under the cursor: the next send replies to it",
            EVERYWHERE,
            &[],
        )
        .palette("reply"),
        Row::new(
            "send the message under the cursor again, one of ours that failed",
            EVERYWHERE,
            &[],
        )
        .palette("send again"),
        Row::new(
            "forward the message under the cursor to a contact by name",
            EVERYWHERE,
            &[],
        )
        .palette("forward"),
        Row::new("mute the open conversation, or unmute it", EVERYWHERE, &[]).palette("mute"),
        Row::new(
            "mark the open peer as compared in person, or no longer",
            EVERYWHERE,
            &[],
        )
        .palette("verify"),
        Row::new(
            "mark the open conversation unread, or read",
            EVERYWHERE,
            &[],
        )
        .palette("mark unread"),
        Row::new(
            "the next photo size: small, phone, original (the photo_size key)",
            EVERYWHERE,
            &[],
        )
        .palette("photo size"),
        Row::new(
            "your position by hand: latitude, longitude, or the map's centre",
            EVERYWHERE,
            &[],
        )
        .palette("position"),
        Row::new(
            "share your position with the open conversation: now, 15m, 1h, 8h, until",
            EVERYWHERE,
            &[],
        )
        .palette("share position"),
        Row::new(
            "stop sharing your position with the open conversation",
            EVERYWHERE,
            &[],
        )
        .palette("stop sharing"),
        Row::new(
            "delete every map on this device, after asking",
            EVERYWHERE,
            &[],
        )
        .palette("delete all maps"),
        Row::new(
            "send a picture to the open conversation: type its path",
            EVERYWHERE,
            &[],
        )
        .palette("send picture"),
        Row::new(
            "send a file to the open conversation: type its path",
            EVERYWHERE,
            &[],
        )
        .palette("send file"),
        Row::new(
            "a contact's code from a picture of its QR: type the picture's path",
            EVERYWHERE,
            &[],
        )
        .palette("code in a picture"),
    ]
};

/// The panes the chords of a pane are looked up in: the command panes take
/// the shared scroll rows too.
pub fn panes_of(pane: crate::ui::Pane) -> Panes {
    match pane {
        crate::ui::Pane::Conversations => LIST.with(Panes::SCROLL),
        crate::ui::Pane::Messages => MESSAGES.with(Panes::SCROLL),
        crate::ui::Pane::Compose => COMPOSE,
        crate::ui::Pane::Search => SEARCH,
        crate::ui::Pane::Map => MAP,
    }
}

/// What `key` runs in `pane` under `ctx`: the most specific chord across
/// loquax's rows and the shared scroll rows, loquax's first among equals.
pub fn resolve(key: &KeyEvent, pane: Panes, ctx: Ctx) -> Option<Action> {
    let own = keymap::best(ROWS, key, pane, ctx);
    let shared = keymap::best(SCROLL_ROWS, key, pane, ctx).map(|(r, c)| (r, Action::Scroll(c)));
    match (own, shared) {
        (Some(own), Some(shared)) if shared.0 > own.0 => Some(shared.1),
        (Some(own), _) => Some(own.1),
        (None, shared) => shared.map(|(_, action)| action),
    }
}

/// The scroll motion `key` makes in a scrolling overlay, the help or a
/// detail view: the shared scroll rows, as in the conversation.
pub fn overlay_scroll(key: &KeyEvent, ctx: Ctx) -> Option<ScrollCmd> {
    keymap::best(SCROLL_ROWS, key, Panes::SCROLL, ctx).map(|(_, cmd)| cmd)
}

/// The row whose palette name is `name`.
pub fn row_named(name: &str) -> Option<&'static Row<Action>> {
    ROWS.iter().find(|r| r.palette == Some(name))
}

/// The chord the palette shows beside `name`: the row's first one this
/// terminal can send.
pub fn palette_key(name: &str, ctx: Ctx) -> Option<&'static str> {
    row_named(name)?
        .chords()
        .find(|c| c.live(Panes::ALL, ctx))
        .map(|c| c.label())
}

/// The help overlay, every row of both tables in the groups of
/// [`GROUPS`]; a row's lost-habit note is the line under it.
pub fn help_groups() -> Vec<HelpGroup> {
    let entry = |keys, desc| HelpEntry { keys, desc };
    GROUPS
        .iter()
        .map(|&title| {
            let mut entries = Vec::new();
            if title == MOVE {
                entries.extend(SCROLL_ROWS.iter().map(|r| entry(r.keys(), r.desc)));
            }
            for row in ROWS.iter().filter(|r| r.group == title) {
                entries.push(entry(row.keys(), row.desc));
                if let Some(note) = row.note {
                    entries.push(entry("", note));
                }
            }
            HelpGroup { title, entries }
        })
        .collect()
}

/// What a footer button runs when clicked: the action of the chord it
/// shows. Nothing for a row that transmits: the mouse sends with the word
/// `send` on the compose box's border and fetches with the mailbox state,
/// and with nothing else (section 5, "What the mouse does").
pub fn footer_action(row: &Row<Action>, chord: &Chord<Action>) -> Option<Action> {
    (!row.tx).then_some(chord.action)
}

/// The footer buttons for `pane` under `ctx`: every row with a footer
/// label and a chord this terminal can send there, its key that chord. A
/// spelled row ([`leviculum_ui::keymap::Row::footer_spelled`]) has a button
/// for each spelling that is live there, its key the whole spelling
/// (`n / N next, previous`), clicked as its first chord; any after the
/// first drop before every other button on a narrow footer.
pub fn footer<A: Copy>(
    pane: Panes,
    ctx: Ctx,
    act: impl Fn(Option<Action>) -> A,
) -> Vec<FooterItem<A>> {
    let mut items = Vec::new();
    for row in ROWS {
        let Some(spec) = row.footer else {
            continue;
        };
        if spec.spelled {
            let mut first = true;
            for (index, spelling) in row.spellings.iter().enumerate() {
                if let Some(chord) = spelling.iter().find(|c| c.live(pane, ctx)) {
                    let (keys, action) = (row.spelling_keys(index), act(footer_action(row, chord)));
                    // The first spelling stays; another is the first thing a
                    // narrow footer drops.
                    items.push(match first {
                        true => FooterItem::action(keys, spec.label, action),
                        false => FooterItem::droppable(keys, spec.label, action, 0),
                    });
                    first = false;
                }
            }
            continue;
        }
        let Some(chord) = row.live_chord(pane, ctx) else {
            continue;
        };
        let action = act(footer_action(row, chord));
        items.push(match spec.drop_rank {
            None => FooterItem::action(chord.label(), spec.label, action),
            Some(rank) => FooterItem::droppable(chord.label(), spec.label, action, rank),
        });
    }
    items
}

/// The chord that does `what` in compose right now.
fn live(what: Action, ctx: Ctx) -> &'static str {
    ROWS.iter()
        .flat_map(|r| r.chords())
        .find(|c| c.action == what && c.live(COMPOSE, ctx))
        .map(|c| c.label())
        .unwrap_or("")
}

/// The chord that sends from compose in this terminal and setting.
pub fn send_chord(ctx: Ctx) -> &'static str {
    live(Action::Run(Command::Send), ctx)
}

/// The chord that breaks the line in compose in this terminal and setting.
pub fn newline_chord(ctx: Ctx) -> &'static str {
    live(Action::Newline, ctx)
}

/// The compose pane's border hint: the live send and line-break chords,
/// `Enter send · Alt-Enter newline` in a plain terminal.
pub fn compose_hint(ctx: Ctx) -> String {
    format!("{} send · {} newline", send_chord(ctx), newline_chord(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::commands::{filter, COMMANDS};
    use leviculum_ui::keymap::{all_best, canonical};

    const PANES: [Panes; 6] = [
        Panes(LIST.0 | Panes::SCROLL.0),
        Panes(MESSAGES.0 | Panes::SCROLL.0),
        COMPOSE,
        OVERLAY,
        SEARCH,
        MAP,
    ];
    /// Every setting: each of the four context bits on and off.
    const CTXS: [Ctx; 16] = {
        let mut out = [Ctx::NONE; 16];
        let bits = [Ctx::ENHANCED.0, ENTER_BREAKS.0, SEARCHING.0, RECORDING.0];
        let mut i = 0;
        while i < 16 {
            let mut ctx = 0;
            let mut b = 0;
            while b < 4 {
                if i & (1 << b) != 0 {
                    ctx |= bits[b];
                }
                b += 1;
            }
            out[i] = Ctx(ctx);
            i += 1;
        }
        out
    };

    /// Every action `key` reaches at the winning rank across both tables.
    fn hits(key: &KeyEvent, pane: Panes, ctx: Ctx) -> Vec<Action> {
        let own = all_best(ROWS, key, pane, ctx);
        let shared: Vec<(u8, Action)> = all_best(SCROLL_ROWS, key, pane, ctx)
            .into_iter()
            .map(|(r, c)| (r, Action::Scroll(c)))
            .collect();
        let top = own.iter().chain(&shared).map(|(r, _)| *r).max();
        let mut out = Vec::new();
        for (rank, action) in own.into_iter().chain(shared) {
            if Some(rank) == top && !out.contains(&action) {
                out.push(action);
            }
        }
        out
    }

    fn every_chord() -> impl Iterator<Item = (Action, KeyEvent, &'static str, Panes, Ctx)> {
        let own = ROWS
            .iter()
            .flat_map(|r| r.chords())
            .map(|c| (c.action, canonical(c), c.label(), c.panes, c.when, c.unless));
        let shared = SCROLL_ROWS.iter().flat_map(|r| r.chords()).map(|c| {
            (
                Action::Scroll(c.action),
                canonical(c),
                c.label(),
                c.panes,
                c.when,
                c.unless,
            )
        });
        own.chain(shared)
            .flat_map(|(action, event, label, panes, when, unless)| {
                PANES.into_iter().flat_map(move |pane| {
                    CTXS.into_iter().filter_map(move |ctx| {
                        let in_force =
                            panes.meets(pane) && ctx.contains(when) && !ctx.meets(unless);
                        in_force.then_some((action, event, label, pane, ctx))
                    })
                })
            })
    }

    /// No chord reaches two actions in one pane: each chord's own key, in
    /// every pane and setting it is in force, resolves to its own row's
    /// action and to nothing else. lnomad's first-match scan could not see a
    /// shadowed chord; this can.
    #[test]
    fn no_chord_resolves_to_two_actions_in_one_pane() {
        for (action, event, label, pane, ctx) in every_chord() {
            let got = hits(&event, pane, ctx);
            assert_eq!(got, [action], "{label} in {pane:?} under {ctx:?}");
            assert_eq!(resolve(&event, pane, ctx), Some(action), "{label}");
        }
    }

    /// Every row is in the help with its derived keys, and every palette
    /// name has a row whose chords run that very command.
    #[test]
    fn every_row_is_in_help_and_every_palette_name_has_a_row() {
        let groups = help_groups();
        let shown: Vec<(&str, &str)> = groups
            .iter()
            .flat_map(|g| g.entries.iter().map(|e| (e.keys, e.desc)))
            .collect();
        for row in ROWS {
            assert!(
                shown.contains(&(row.keys(), row.desc)),
                "{:?} missing from help",
                row.desc
            );
            assert!(GROUPS.contains(&row.group), "{:?}", row.group);
        }
        for row in SCROLL_ROWS {
            assert!(shown.contains(&(row.keys(), row.desc)), "{:?}", row.desc);
        }
        for entry in COMMANDS {
            let row = row_named(entry.name)
                .unwrap_or_else(|| panic!("palette name {:?} has no row", entry.name));
            for chord in row.chords() {
                assert_eq!(chord.action, Action::Run(entry.command), "{}", entry.name);
            }
        }
        for row in ROWS {
            if let Some(name) = row.palette {
                assert!(
                    COMMANDS.iter().any(|e| e.name == name),
                    "row {:?} names {name:?}, not in the palette",
                    row.desc
                );
            }
        }
    }

    /// The quiet keyboard: the one transmitting chord a reader can type by
    /// accident is `Enter` in compose. `v` sends a voice message only
    /// while one records (1276): the bar is in the field's place then, so
    /// no typing reaches it, and the reader started the recording on
    /// purpose with `Alt-v`.
    #[test]
    fn the_only_single_keystroke_that_transmits_is_send_in_compose() {
        let mut single = Vec::new();
        for row in ROWS.iter().filter(|r| r.tx) {
            for chord in row.chords().filter(|c| c.is_single_keystroke()) {
                single.push((chord.label(), chord.panes, chord.when));
            }
        }
        assert_eq!(
            single,
            [("Enter", COMPOSE, Ctx::NONE), ("v", COMPOSE, RECORDING)]
        );
    }

    /// The recording's keys (1276): `Alt-v` starts and, once it records,
    /// sends; `v` is a letter until then and sends after; `Esc` throws it
    /// away in compose and still backs out of the messages; `p` and
    /// `Space` are the bar's alone.
    #[test]
    fn the_recordings_keys_belong_to_the_bar_while_it_records() {
        let k = |code, mods| KeyEvent::new(code, mods);
        let (v, alt_v) = (
            k(KeyCode::Char('v'), KeyModifiers::NONE),
            k(KeyCode::Char('v'), KeyModifiers::ALT),
        );
        let esc = k(KeyCode::Esc, KeyModifiers::NONE);
        let rec = |key| Some(Action::Record(key));
        assert_eq!(resolve(&alt_v, COMPOSE, Ctx::NONE), rec(RecKey::Start));
        assert_eq!(resolve(&v, COMPOSE, Ctx::NONE), None, "a letter");
        assert_eq!(resolve(&v, COMPOSE, RECORDING), rec(RecKey::Send));
        assert_eq!(resolve(&alt_v, COMPOSE, RECORDING), rec(RecKey::Send));
        assert_eq!(resolve(&esc, COMPOSE, RECORDING), rec(RecKey::Discard));
        assert_eq!(resolve(&esc, COMPOSE, Ctx::NONE), Some(Action::Back));
        assert_eq!(
            resolve(&esc, panes_of(crate::ui::Pane::Messages), RECORDING),
            Some(Action::Back)
        );
        let p = k(KeyCode::Char('p'), KeyModifiers::NONE);
        let space = k(KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(resolve(&p, COMPOSE, RECORDING), rec(RecKey::Pause));
        assert_eq!(resolve(&space, COMPOSE, RECORDING), rec(RecKey::Play));
        assert_eq!(resolve(&p, COMPOSE, Ctx::NONE), None);
        // The send chord stays the send chord: the model sends the
        // recording with it.
        let enter = k(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            resolve(&enter, COMPOSE, RECORDING),
            Some(Action::Run(Command::Send))
        );
    }

    /// A chord only an enhanced terminal sends always has a sibling every
    /// terminal sends, in the same row.
    #[test]
    fn every_enhanced_chord_has_a_plain_one_beside_it() {
        for row in ROWS {
            if row.chords().any(|c| c.enhanced) {
                assert!(
                    row.chords().any(|c| !c.enhanced),
                    "{:?} only has enhanced chords",
                    row.desc
                );
            }
        }
    }

    /// `:` opens the palette in a command pane, `q` narrows it to `quit`
    /// alone, so `Enter` quits.
    #[test]
    fn colon_q_enter_quits() {
        let colon = KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(resolve(&colon, PANES[0], Ctx::NONE), Some(Action::Palette));
        assert_eq!(filter("q")[0].command, Command::Quit);
    }

    /// The hint names the chord live in this terminal and setting, never
    /// both worlds.
    #[test]
    fn the_compose_hint_follows_the_terminal_and_the_switch() {
        assert_eq!(compose_hint(Ctx::NONE), "Enter send · Alt-Enter newline");
        assert_eq!(
            compose_hint(Ctx::ENHANCED),
            "Enter send · Shift-Enter newline"
        );
        assert_eq!(compose_hint(ENTER_BREAKS), "Alt-Enter send · Enter newline");
        assert_eq!(
            compose_hint(Ctx(Ctx::ENHANCED.0 | ENTER_BREAKS.0)),
            "Ctrl-Enter send · Enter newline"
        );
    }

    /// `Ctrl-Enter` sends in every setting and never breaks the line.
    #[test]
    fn ctrl_enter_always_sends() {
        let ctrl_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        for ctx in CTXS {
            assert_eq!(
                resolve(&ctrl_enter, COMPOSE, ctx),
                Some(Action::Run(Command::Send)),
                "{ctx:?}"
            );
        }
    }
}
