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
pub const COMMANDS: [PaletteEntry; 35] = [
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
    entry("rename", Command::Rename),
    entry("react", Command::React),
    entry("block", Command::Block),
    entry("unblock", Command::Unblock),
    entry("blocked", Command::Blocked),
    entry("archive", Command::Archive),
    entry("archived", Command::Archived),
    entry("import", Command::Import),
    entry("read receipts", Command::ReadReceipts),
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
