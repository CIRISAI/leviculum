//! The command palette's names: the one table a messenger frontend opens
//! with `:` or `Alt-x` and filters as the reader types
//! (`docs/src/concepts/loquax-ui.md`, section 1: "a command palette layered on
//! top, no modes").
//!
//! One table, so the terminal palette and the desktop GUI's offer the same
//! words for the same actions. The program maps a [`Command`] to what it
//! does; this module only names them and filters them. Each name is also a
//! row of the program's keymap ([`crate::keymap::Row::palette`]), which is
//! where its keys come from.
//!
//! `quit` is the only name with a `q` in it, so `:`, `q`, `Enter` quits as a
//! vi reader expects (`docs/src/concepts/loquax-keys-and-layout.md`, section
//! 3); a test holds it.

use alloc::vec::Vec;

/// What a palette entry runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// Send the compose line to the selected conversation.
    Send,
    /// Drain the propagation node's mailbox.
    Fetch,
    /// Leave the program.
    Quit,
    /// Show the keybindings.
    Help,
    /// Move the focus to the conversation list.
    FocusConversations,
    /// Move the focus to the message list.
    FocusMessages,
    /// Move the focus to the compose line.
    FocusCompose,
    /// Select the next conversation.
    NextConversation,
    /// Select the previous conversation.
    PreviousConversation,
    /// Select the next conversation with unread messages.
    NextUnread,
    /// Select the previous conversation with unread messages.
    PreviousUnread,
    /// What is known about the message under the cursor.
    Details,
    /// Draw the whole screen again.
    Redraw,
    /// Swap what `Enter` does in compose: send, or break the line.
    SendOnEnter,
    /// Turn mouse capture off, so the terminal's own selection works, and on
    /// again.
    Mouse,
    /// Swap the light and the dark colours for the session.
    Theme,
    /// Open the search over the conversations and their messages, or put
    /// the cursor back in its field.
    Search,
    /// Delete the message under the cursor from this device, after asking.
    DeleteMessage,
    /// Delete the open conversation from this device, after asking.
    DeleteConversation,
    /// Pin the open conversation above the others, or unpin it.
    Pin,
    /// Show the peers' positions on the map, in the conversation's place.
    Map,
    /// Load the map's region of the visible area.
    LoadMapArea,
    /// Bring the Browse tab, the NomadNet pages, to the front (Loquax
    /// 1369), where the host has it.
    Browse,
    /// Bring the Rooms tab, Reticulum Relay Chat on a hub, to the front
    /// (Loquax 1379), where the host has it.
    Rooms,
    /// Bring the Explore tab, the pages and the public rooms under one
    /// button, to the front (Loquax 1578).
    Explore,
    /// Choose how the map is drawn, from the ways the frontend offers.
    MapDrawing,
    /// Give the open conversation's peer a name of the reader's own, or
    /// clear it: the palette takes the name as text.
    Rename,
    /// React to the message under the cursor: a quick emoji or a short text
    /// of the reader's own.
    React,
    /// Block the open conversation's peer: what it sends is kept, not shown.
    Block,
    /// Lift the block on the open conversation's peer.
    Unblock,
    /// List the blocked conversations instead of the others, and back.
    Blocked,
    /// Move the open conversation into the archive, or back to the list.
    Archive,
    /// List the archived conversations instead of the others, and back.
    Archived,
    /// Bring over contacts and history from another messenger found in its
    /// default place.
    Import,
    /// Turn sending read receipts off, and on again: off, none is sent and
    /// none is shown.
    ReadReceipts,
    /// Start a conversation: the palette takes a friend's code or address
    /// as text.
    NewConversation,
    /// Add a contact: a friend's code, then the name for it.
    AddContact,
    /// Show our own contact code and its QR, for a friend to add us.
    MyCode,
    /// Make a group (Loquax 1307): tick the friends, then its name.
    NewGroup,
    /// Join the group whose invitation the open conversation holds.
    JoinGroup,
    /// Show the open group's members.
    Members,
    /// Leave the open group, after asking.
    LeaveGroup,
    /// Remove a member from the open group, its admin's: the palette takes
    /// the member's name.
    RemoveMember,
    /// Add members to the open group, its admin's (Loquax 1307 G2): tick
    /// the friends, then Enter.
    AddMembers,
    /// Delete the open group for everyone, its admin's, after asking
    /// (Loquax 1307 pass four).
    DeleteGroup,
    /// Make the open group again with the same people, once the reader
    /// left it or it ended (Loquax 1307 pass four): Enter keeps its name.
    StartAgain,
    /// Answer the message under the cursor: the next send replies to it
    /// (Loquax 1371).
    Reply,
    /// Send the message under the cursor again, one of ours that failed.
    SendAgain,
    /// Forward the message under the cursor: the palette takes a friend's
    /// name.
    Forward,
    /// Mute the open conversation until unmuted, or unmute it.
    Mute,
    /// Mark the open peer's identity as compared in person, or no longer.
    Verify,
    /// Mark the open conversation unread, or read.
    MarkUnread,
    /// The next of the three photo sizes a sent picture is scaled to.
    PhotoSize,
    /// Set the own position by hand: the palette takes latitude and
    /// longitude, or the map's centre.
    Position,
    /// Share the own position with the open conversation, once or for a
    /// while.
    SharePosition,
    /// Stop sharing the position with the open conversation.
    StopSharing,
    /// Delete every map on this device, after asking.
    DeleteAllMaps,
    /// Send a picture to the open conversation: the palette takes its
    /// path.
    SendPicture,
    /// Send a file to the open conversation: the palette takes its path.
    SendFile,
    /// Read a friend's code from a picture of its QR: the palette takes
    /// the picture's path.
    CodeFromPicture,
    /// The settings' nine sections (Loquax 1450), as the phone's menu and
    /// the window's gear open them.
    Settings,
}

/// One palette row: the words the reader filters on, and what it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaletteEntry {
    /// The name as typed and shown.
    pub name: &'static str,
    /// What it runs.
    pub command: Command,
}

/// The table, in the order the palette lists it unfiltered. A first user's
/// two commands stand right under `send`, where an open palette shows them
/// (1263), and the code a friend adds us by right after them (1279).
pub const COMMANDS: [PaletteEntry; 61] = [
    entry("send", Command::Send),
    entry("new conversation", Command::NewConversation),
    entry("add contact", Command::AddContact),
    entry("my code", Command::MyCode),
    entry("fetch", Command::Fetch),
    entry("quit", Command::Quit),
    entry("help", Command::Help),
    entry("focus conversations", Command::FocusConversations),
    entry("focus messages", Command::FocusMessages),
    entry("focus compose", Command::FocusCompose),
    entry("next conversation", Command::NextConversation),
    entry("previous conversation", Command::PreviousConversation),
    entry("next unread", Command::NextUnread),
    entry("previous unread", Command::PreviousUnread),
    entry("details", Command::Details),
    entry("redraw", Command::Redraw),
    entry("send on enter", Command::SendOnEnter),
    entry("mouse", Command::Mouse),
    entry("theme", Command::Theme),
    entry("search", Command::Search),
    entry("delete message", Command::DeleteMessage),
    entry("delete conversation", Command::DeleteConversation),
    entry("pin", Command::Pin),
    entry("map", Command::Map),
    entry("load map area", Command::LoadMapArea),
    entry("map drawing", Command::MapDrawing),
    entry("browse", Command::Browse),
    entry("rooms", Command::Rooms),
    entry("explore", Command::Explore),
    entry("rename", Command::Rename),
    entry("react", Command::React),
    entry("block", Command::Block),
    entry("unblock", Command::Unblock),
    entry("blocked", Command::Blocked),
    entry("archive", Command::Archive),
    entry("archived", Command::Archived),
    entry("import", Command::Import),
    entry("read receipts", Command::ReadReceipts),
    entry("new group", Command::NewGroup),
    entry("join group", Command::JoinGroup),
    entry("members", Command::Members),
    entry("leave group", Command::LeaveGroup),
    entry("remove member", Command::RemoveMember),
    entry("add members", Command::AddMembers),
    entry("delete group", Command::DeleteGroup),
    entry("start again", Command::StartAgain),
    entry("reply", Command::Reply),
    entry("send again", Command::SendAgain),
    entry("forward", Command::Forward),
    entry("mute", Command::Mute),
    entry("verify", Command::Verify),
    entry("mark unread", Command::MarkUnread),
    entry("photo size", Command::PhotoSize),
    entry("position", Command::Position),
    entry("share position", Command::SharePosition),
    entry("stop sharing", Command::StopSharing),
    entry("delete all maps", Command::DeleteAllMaps),
    entry("send picture", Command::SendPicture),
    entry("send file", Command::SendFile),
    entry("code in a picture", Command::CodeFromPicture),
    entry("settings", Command::Settings),
];

/// The entry that runs `command`.
pub fn name_of(command: Command) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|e| e.command == command)
        .map(|e| e.name)
}

const fn entry(name: &'static str, command: Command) -> PaletteEntry {
    PaletteEntry { name, command }
}

/// Whether `query` is a subsequence of `name`, ignoring ASCII case: the
/// palette's fuzzy match, so `fc` finds `focus compose`.
pub fn fuzzy_match(query: &str, name: &str) -> bool {
    let mut rest = name.chars();
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|q| rest.any(|n| n.eq_ignore_ascii_case(&q)))
}

/// The entries `query` matches: the one it names exactly first, then those
/// it begins, then the rest, each in table order. Without the ranking
/// `:pin` and `Enter` ran `previous conversation`, whose letters hold `p`,
/// `i` and `n` in order and which stands first in the table.
pub fn filter(query: &str) -> Vec<&'static PaletteEntry> {
    let query = query.trim();
    let mut found: Vec<&'static PaletteEntry> = COMMANDS
        .iter()
        .filter(|e| fuzzy_match(query, e.name))
        .collect();
    found.sort_by_key(|e| {
        let exact = e.name.eq_ignore_ascii_case(query);
        let prefix = e
            .name
            .get(..query.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(query));
        (!exact, !prefix)
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_name_or_its_start_ranks_before_a_scattered_match() {
        let names = |query: &str| -> Vec<&str> { filter(query).iter().map(|e| e.name).collect() };
        assert_eq!(names("pin")[0], "pin");
        assert!(names("pin").contains(&"previous conversation"));
        assert_eq!(names("delete m")[0], "delete message");
        assert_eq!(names("PIN")[0], "pin");
        // A scattered match alone keeps the table's order.
        assert_eq!(
            names("fc"),
            [
                "fetch",
                "focus conversations",
                "focus messages",
                "focus compose"
            ]
        );
    }

    #[test]
    fn an_empty_query_lists_the_whole_table_in_order() {
        let all: Vec<&str> = filter("").iter().map(|e| e.name).collect();
        assert_eq!(all, COMMANDS.map(|e| e.name));
    }

    #[test]
    fn a_subsequence_matches_and_a_reordering_does_not() {
        assert!(fuzzy_match("fc", "focus compose"));
        assert!(fuzzy_match("FOCO", "focus compose"));
        assert!(fuzzy_match("focus m", "focus messages"));
        assert!(!fuzzy_match("cf", "fetch"));
        assert!(
            fuzzy_match("fc", "fetch"),
            "a subsequence, not a word start"
        );
        let hits: Vec<Command> = filter("foc").iter().map(|e| e.command).collect();
        assert_eq!(
            hits,
            [
                Command::FocusConversations,
                Command::FocusMessages,
                Command::FocusCompose
            ]
        );
        assert_eq!(filter("qu")[0].command, Command::Quit);
    }

    /// `:q` then `Enter` runs the first match, and `quit` is the only name
    /// a `q` matches.
    #[test]
    fn q_alone_finds_quit_first() {
        let hits: Vec<Command> = filter("q").iter().map(|e| e.command).collect();
        assert_eq!(hits, [Command::Quit]);
    }
}
