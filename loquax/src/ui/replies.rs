//! Replies and forwards: a message sent with a link to an earlier one, or
//! a copy of one into another conversation.
//!
//! A reply carries `FIELD_REPLY_TO` with the target's id and
//! `FIELD_REPLY_QUOTE` with what of it the reader saw
//! ([`leviculum_lxmf::reply`]), the shape Columba, MeshChatX and Ratspeak
//! write and read; its text is the content, so a client without replies
//! shows it as a message. A bubble shows the quote above the reply: the
//! target's own text when this device holds it, else the quote the sender
//! put in.
//!
//! A forward is the text again, and of the fields only those that are the
//! message's content: attachments, an image, audio, the renderer. What
//! describes the first sender (telemetry, the icon, a ticket) or another
//! conversation (a reply's target) would say something false in the new
//! one. No deployed client marks a forward on the wire, so it goes
//! unmarked.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::constants::{
    FIELD_AUDIO, FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE, FIELD_FILE_ATTACHMENTS, FIELD_IMAGE,
    FIELD_RENDERER,
};
use leviculum_lxmf::reply::Reply;
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;

use super::{Message, StoreWrite, UiEffect, UiModel};
use crate::home::wire::MarkFlag;

/// The longest quote sent, in characters: enough to know the message by,
/// short enough not to double a long one on the air.
pub const QUOTE_CHARS: usize = 120;

/// The fields a forward copies ([module docs](self)).
pub const FORWARDED_FIELDS: [i64; 4] = [
    FIELD_FILE_ATTACHMENTS,
    FIELD_IMAGE,
    FIELD_AUDIO,
    FIELD_RENDERER,
];

/// What a reply quotes of `body`: its first line, cut at [`QUOTE_CHARS`].
///
/// A relay's post (1482) is quoted by its writer and words, `"Anna: Hallo"`,
/// not by the first line of the relay's prefix, the writer's name alone.
pub fn quote(body: &str) -> String {
    let named = leviculum_lxmf::relay::prefix(body)
        .map(|writer| alloc::format!("{}: {}", writer.name, writer.text));
    let body = named.as_deref().unwrap_or(body);
    let first = body.lines().next().unwrap_or("").trim();
    match first.chars().count() > QUOTE_CHARS {
        true => {
            let mut cut: String = first.chars().take(QUOTE_CHARS - 1).collect();
            cut.push('\u{2026}');
            cut
        }
        false => first.into(),
    }
}

/// The fields of a forward of a message that came with `fields`.
pub fn forwarded(fields: &[Field]) -> Vec<Field> {
    fields
        .iter()
        .filter(|(key, _)| FORWARDED_FIELDS.contains(key))
        .cloned()
        .collect()
}

impl Message {
    /// The message this one replies to, read from its fields.
    pub fn reply(&self) -> Option<Reply> {
        Reply::from_fields(&self.fields)
    }
}

/// What a bubble shows above a reply: the quoted text, and whether the
/// quoted message is ours (`None` when this device does not hold it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quoted {
    pub text: String,
    pub ours: Option<bool>,
    /// The target's id, for a tap that goes to it.
    pub to: [u8; 32],
}

impl<B> UiModel<B> {
    /// The quote above message `m` of conversation `c`, if it is a reply.
    pub fn quoted(&self, c: usize, m: usize) -> Option<Quoted> {
        let conversation = self.conversations.get(c)?;
        let reply = conversation.messages.get(m)?.reply()?;
        let target = conversation
            .messages
            .iter()
            .find(|t| t.id == Some(reply.to));
        // A target this device does not hold (1482): a relay passes the
        // writer's own copy's id through. Its quote names its writer when
        // the relay's prefix is in it; without a quote, a stub says the
        // message is not here, never an empty block.
        Some(Quoted {
            text: match (target, reply.quote) {
                (Some(target), _) => quote(&target.body),
                (None, Some(text)) if leviculum_lxmf::relay::prefix(&text).is_some() => {
                    quote(&text)
                }
                (None, Some(text)) => text,
                (None, None) => String::from(super::strings::StrId::QuoteNotHere.text(self.lang)),
            },
            ours: target.map(|t| t.outbound),
            to: reply.to,
        })
    }

    /// Send `body` to `peer` as a reply to the message `target`, quoting
    /// it when this device holds it.
    pub(super) fn reply_to(
        &mut self,
        peer: [u8; 16],
        target: [u8; 32],
        body: String,
    ) -> Vec<UiEffect> {
        let body = super::outgoing(body);
        if body.is_empty() {
            return Vec::new();
        }
        let index = self.conversation(peer);
        self.clear_draft(peer);
        let quote = self.conversations[index]
            .messages
            .iter()
            .find(|m| m.id == Some(target))
            .map(|m| quote(&m.body));
        let fields = Reply { to: target, quote }.fields();
        self.select(index);
        self.send_fields(index, body, fields)
    }

    /// Send a copy of the message `key` of `from`'s conversation to `to`.
    pub(super) fn forward(&mut self, from: [u8; 16], key: u64, to: [u8; 16]) -> Vec<UiEffect> {
        let Some(message) = self
            .conversations
            .iter()
            .find(|c| c.peer == from)
            .and_then(|c| c.messages.iter().find(|m| m.activity == key))
        else {
            return Vec::new();
        };
        let body = message.body.clone();
        let fields = forwarded(&message.fields);
        if body.trim().is_empty() && fields.is_empty() {
            return Vec::new();
        }
        let index = self.conversation(to);
        self.send_fields(index, body, fields)
    }

    /// Send the message `key` of `peer`'s conversation again, one of ours
    /// that was rejected or given up: it leaves this device's store and the
    /// screen (as a delete would, without its line), and a new message with
    /// its text and its fields goes out. The `lev` pair it carried goes
    /// with it: the new one gets the mark of its own send. Anything else,
    /// or a key no longer there, does nothing.
    pub(super) fn send_again(&mut self, peer: [u8; 16], key: u64) -> Vec<UiEffect> {
        let Some(c) = self.conversations.iter().position(|c| c.peer == peer) else {
            return Vec::new();
        };
        let Some(message) = self.conversations[c]
            .messages
            .iter()
            .find(|m| m.activity == key)
        else {
            return Vec::new();
        };
        let failed = matches!(
            message.delivery,
            Some(DeliveryState::Failed | DeliveryState::Rejected)
        );
        if !message.outbound || !failed {
            return Vec::new();
        }
        let body = message.body.clone();
        let fields: Vec<Field> = message
            .fields
            .iter()
            .filter(|(k, _)| *k != FIELD_CUSTOM_TYPE && *k != FIELD_CUSTOM_DATA)
            .cloned()
            .collect();
        if let Some(id) = self.remove_message(peer, key) {
            self.forgotten.push(StoreWrite::ForgetMessage { key });
            // What it saved goes with it: the new one has its own key (1662).
            self.saved_goes(peer, Some(key));
            // The home's other devices drop the failed one too.
            if let (true, Some(id)) = (self.home_client, id) {
                self.mark_out(MarkFlag::Deleted, alloc::vec![id]);
            }
        }
        let index = self.conversation(peer);
        self.select(index);
        self.send_fields(index, body, fields)
    }

    /// Send `body` with `fields` in conversation `index`: a message of ours,
    /// delivered as any other. The view of that conversation goes to it.
    pub(super) fn send_fields(
        &mut self,
        index: usize,
        body: String,
        fields: Vec<Field>,
    ) -> Vec<UiEffect> {
        let mut message = Message::ours(body.clone(), self.now);
        message.fields = fields.clone();
        let activity = self.bump();
        message.activity = activity;
        let conversation = &mut self.conversations[index];
        let to = conversation.peer;
        conversation.messages.push(message);
        conversation.activity = activity;
        let at = conversation.messages.len() - 1;
        self.touch(index, at);
        if index == self.selected && !body.is_empty() {
            self.message_cursor = at;
            self.scroll_top = None;
        }
        self.sending.push_back((index, at));
        let now = self.elapsed_ms;
        if let Some(sync) = self.sync.as_mut() {
            sync.on(crate::sync::SyncEvent::Sent, now);
        }
        alloc::vec![UiEffect::Send { to, body, fields }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::constants::{
        FIELD_ICON_APPEARANCE, FIELD_REPLY_TO, FIELD_TELEMETRY, FIELD_TICKET,
    };

    #[test]
    fn a_quote_is_the_first_line_cut() {
        assert_eq!(quote("  hello\nworld"), "hello");
        let long = "x".repeat(QUOTE_CHARS + 5);
        let cut = quote(&long);
        assert_eq!(cut.chars().count(), QUOTE_CHARS);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(quote(""), "");
    }

    #[test]
    fn a_forward_copies_the_content_fields_only() {
        let fields = alloc::vec![
            (FIELD_TELEMETRY, alloc::vec![0xc0]),
            (FIELD_IMAGE, alloc::vec![0x01]),
            (FIELD_ICON_APPEARANCE, alloc::vec![0xc0]),
            (FIELD_TICKET, alloc::vec![0xc0]),
            (FIELD_REPLY_TO, alloc::vec![0xc0]),
            (FIELD_FILE_ATTACHMENTS, alloc::vec![0x02]),
        ];
        assert_eq!(
            forwarded(&fields),
            alloc::vec![
                (FIELD_IMAGE, alloc::vec![0x01]),
                (FIELD_FILE_ATTACHMENTS, alloc::vec![0x02])
            ]
        );
    }
}
