//! A group without a protocol (1482): a relay bot that hands every member
//! a 1:1 copy of every member's message, its writer named only in the text,
//! `Name\n<hash>\n` in front ([`leviculum_lxmf::relay`]). "ReticulumNet NL"
//! is one, SebastianObi's LXMF-Tools `lxmf_distribution_group_extended` on
//! its default configuration.
//!
//! - A chat counts as relayed once [`RELAYED_AFTER`] of its peer's messages
//!   named another writer: one prefix could be a message someone pasted.
//! - There a bubble names its writer, from the relay's `0xB5` when it sends
//!   one, else from the prefix: the reader's own word for the address, else
//!   the name the relay gave. The bubble shows the words after the prefix.
//!   The top bar says the chat is a group via a relay. The Info names the
//!   writer with the address's 4-hex tail (1452: hashes only there).
//! - A prefix the relay's operator changed is no prefix here: the posts
//!   show as they came, and no writer is named.
//! - What a relay hands back of a receipt a writer before 1482 sent it is
//!   the writer's name over no words with nothing but the namespace: no
//!   post ([`noise`]).

use alloc::string::String;

use leviculum_lxmf::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE};
use leviculum_lxmf::relay::{self, FIELD_RELAY_HASH, FIELD_RELAY_SOURCE, FIELD_RELAY_TYPE};
use leviculum_lxmf::Field;

use super::strings::{Arg, Lang, StrId, Text};
use super::{Conversation, Message, UiModel};
use crate::names::Names;

/// How many of a peer's messages must name another writer before its chat
/// counts as relayed.
pub const RELAYED_AFTER: usize = 2;

/// Whether `conversation` is a relay's: [`RELAYED_AFTER`] or more of its
/// peer's messages name another writer.
pub fn is_relayed(conversation: &Conversation) -> bool {
    conversation
        .messages
        .iter()
        .filter(|m| !m.outbound && writer_of(m, &conversation.peer).is_some())
        .take(RELAYED_AFTER)
        .count()
        >= RELAYED_AFTER
}

/// The writer a message from `peer` names, when it names another.
pub fn writer_of<'a>(message: &'a Message, peer: &[u8; 16]) -> Option<relay::Writer<'a>> {
    relay::writer(&message.body, &message.fields, peer)
}

/// Whether a message from `source` is what a relay makes of a receipt: it
/// names another writer, has no words after the prefix, and carries no
/// field but a custom pair (and the relay's own fields).
pub fn noise(body: &[u8], fields: &[Field], source: &[u8; 16]) -> bool {
    let Ok(body) = core::str::from_utf8(body) else {
        return false;
    };
    let Some(writer) = relay::writer(body, fields, source) else {
        return false;
    };
    writer.text.trim().is_empty()
        && fields.iter().any(|(key, _)| *key == FIELD_CUSTOM_TYPE)
        && fields.iter().all(|(key, _)| {
            matches!(
                *key,
                FIELD_CUSTOM_TYPE
                    | FIELD_CUSTOM_DATA
                    | FIELD_RELAY_SOURCE
                    | FIELD_RELAY_HASH
                    | FIELD_RELAY_TYPE
            )
        })
}

/// The content of a reaction in `conversation` whose target this device
/// does not hold: a relay passes the writer's target through, the id of
/// the writer's own copy. In a relay's chat such a reaction is said in
/// words where it came, never a chip on nothing; elsewhere a reaction
/// that came before its target waits for it. `None` for every other
/// message.
pub fn orphan_reaction(conversation: &Conversation, message: &Message) -> Option<String> {
    let (to, content) = message.reaction().filter(|_| message.is_reaction())?;
    (!conversation.messages.iter().any(|m| m.id == Some(to))).then_some(content)
}

/// The terminal's words for a post in a relayed chat: `"Anna: Hallo"`, as
/// the terminal names a group's writer in the line itself, and `"Ben:
/// reacted 👍 to a message that is not here"` for a reaction whose target is
/// not here. `None` for ours and for a post that names no writer.
pub fn terminal_line(
    names: &Names,
    lang: Lang,
    conversation: &Conversation,
    message: &Message,
) -> Option<String> {
    if message.outbound {
        return None;
    }
    let writer = writer_of(message, &conversation.peer)?;
    let name = writer_name(names, &writer, lang);
    let words = match orphan_reaction(conversation, message) {
        Some(content) => Text::Key(
            StrId::ReactedElsewhere,
            alloc::vec::Vec::from([Arg::Str(content)]),
        )
        .resolve(lang),
        None => writer.text.into(),
    };
    Some(alloc::format!("{name}: {words}"))
}

/// The name a relayed post's writer goes by: the reader's own word for the
/// address, else the relay's, else the address's label.
pub fn writer_name(names: &Names, writer: &relay::Writer<'_>, lang: Lang) -> String {
    match names.shown(&writer.address) {
        Some(shown) => shown.into(),
        None if !writer.name.is_empty() => writer.name.clone(),
        None => names.label(&writer.address, lang),
    }
}

impl<B> UiModel<B> {
    /// [`writer_name`] by the reader's names.
    pub fn relayed_name(&self, writer: &relay::Writer<'_>) -> String {
        writer_name(&self.names, writer, self.lang)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::lev::Lev;

    const BOT: [u8; 16] = [0xb0; 16];

    fn copy(name: &str, text: &str) -> String {
        alloc::format!("{name}\n<{}>\n{text}", crate::address::to_hex(&[0xa1; 16]))
    }

    #[test]
    fn a_relayed_receipt_is_noise_and_a_post_is_not() {
        let lev = Lev::mark().fields().to_vec();
        assert!(noise(copy("Ben", "").as_bytes(), &lev, &BOT));
        assert!(noise(copy("Ben", " \n").as_bytes(), &lev, &BOT));
        assert!(!noise(copy("Ben", "Moin").as_bytes(), &lev, &BOT), "words");
        assert!(
            !noise(copy("Ben", "").as_bytes(), &[], &BOT),
            "no namespace"
        );
        let reaction = alloc::vec![(leviculum_lxmf::constants::FIELD_REACTION, alloc::vec![0x80])];
        let mut both = lev.clone();
        both.extend(reaction);
        assert!(
            !noise(copy("Ben", "").as_bytes(), &both, &BOT),
            "a reaction"
        );
        assert!(!noise(b"", &lev, &BOT), "no prefix: the bot's own");
    }
}
