//! Reactions: an emoji attached later to an earlier message, the LXMF
//! `FIELD_REACTION` ([`leviculum_lxmf::reaction`]).
//!
//! A reaction is a message of its own: it is stored, deduplicated and
//! delivered as every message is, and it is never drawn as a block. The
//! layout folds it away ([`super::layout::folded`]) and draws what the
//! conversation's reactions say about a message on a row under that
//! message's block: each reaction once, with how many sent it, the reader's
//! own in brackets. One whose target the screen does not hold stays folded
//! until the target arrives or its page is loaded; under propagation a
//! reaction can well arrive before what it reacts to.
//!
//! What a reaction message carries besides the field is not shown: Ratspeak
//! 1.0.25 wrote `Reacted to your message with …` into the content for
//! clients without reactions, and a second line saying the same would be
//! noise. What loquax sends carries the field alone, an empty content and
//! title, as Columba and MeshChatX send it.
//!
//! The palette's `react` and `e` on a message open the picker: the six
//! quick reactions Columba offers, chosen by their digit, or any short text
//! typed and sent with `Enter`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_lxmf::constants::FIELD_REACTION;
use leviculum_lxmf::reaction::Reaction;
use leviculum_ui::input::InputBuffer;
use leviculum_ui::text::{fit_width, truncate_to_cols};
use leviculum_ui::toast::ToastKind;
use leviculum_ui::tokens::Tokens;
use leviculum_ui::{RLine, RStyle};

use super::contacts::Ask;
use super::layout::{painted, NARROW};
use super::{push, Conversation, Message, Palette, Signature, UiEffect, UiModel};

/// The quick reactions, in the picker's order: Columba's six
/// (`REACTION_EMOJIS`, torlando-tech/columba `9222e38`,
/// `app/src/main/java/network/columba/app/ui/components/ReactionComponents.kt:80-88`),
/// the same strings, so one of ours and one of Columba's on a message count
/// together.
pub const QUICK: [&str; 6] = [
    "\u{1F44D}",
    "\u{2764}\u{FE0F}",
    "\u{1F602}",
    "\u{1F62E}",
    "\u{1F622}",
    "\u{1F621}",
];

/// The reaction row of the Android app's long press: Signal's default six
/// (Lew's reference, 2026-10-29), the same strings, U+FE0F included, until
/// the reader's own most used take their place.
pub const SIGNAL_QUICK: [&str; 6] = [
    "\u{2764}\u{FE0F}",
    "\u{1F44D}",
    "\u{1F44E}",
    "\u{1F602}",
    "\u{1F62E}",
    "\u{1F622}",
];

/// The longest reaction drawn, in characters; Ratspeak and MeshChatX cut at
/// the same 16.
pub const MAX_CHARS: usize = 16;

/// A reaction's content as drawn: control characters dropped, cut at
/// [`MAX_CHARS`]. `None` when nothing is left.
pub fn sanitise(content: &str) -> Option<String> {
    let kept: String = content
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHARS)
        .collect();
    let kept = kept.trim();
    (!kept.is_empty()).then(|| kept.to_string())
}

impl Message {
    /// The reaction this message is, read from its fields: the target and
    /// the content as drawn.
    pub fn reaction(&self) -> Option<([u8; 32], String)> {
        let (_, value) = self.fields.iter().find(|(key, _)| *key == FIELD_REACTION)?;
        let reaction = Reaction::decode_field_value(value)?;
        Some((reaction.to, sanitise(&reaction.content)?))
    }

    /// Whether this message is drawn as a reaction under its target and
    /// not as a block. One whose signature does not hold is a block, so
    /// what the reader is warned about is on screen.
    pub fn is_reaction(&self) -> bool {
        self.signature != Signature::Invalid && self.reaction().is_some()
    }
}

/// One reaction on a message: the content, how many sent it, and whether
/// the reader did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub content: String,
    pub count: usize,
    pub own: bool,
}

/// The reactions the conversation holds, by the id of the message they
/// react to, each target's in the order they first came. The same content
/// from the same side counts once, however often it came.
pub fn gather(conversation: &Conversation) -> BTreeMap<[u8; 32], Vec<Chip>> {
    let mut by_target: BTreeMap<[u8; 32], Vec<Chip>> = BTreeMap::new();
    let mut seen: BTreeSet<([u8; 32], bool, String)> = BTreeSet::new();
    for message in &conversation.messages {
        if message.signature == Signature::Invalid {
            continue;
        }
        let Some((to, content)) = message.reaction() else {
            continue;
        };
        if !seen.insert((to, message.outbound, content.clone())) {
            continue;
        }
        let chips = by_target.entry(to).or_default();
        match chips.iter_mut().find(|c| c.content == content) {
            Some(chip) => {
                chip.count += 1;
                chip.own |= message.outbound;
            }
            None => chips.push(Chip {
                content,
                count: 1,
                own: message.outbound,
            }),
        }
    }
    by_target
}

/// `👍 2  [❤️]`: one chip a reaction, its count when more than one sent it,
/// the reader's own in brackets.
pub fn words(chips: &[Chip]) -> Vec<(String, bool)> {
    chips
        .iter()
        .map(|chip| {
            let text = match chip.count {
                1 => chip.content.clone(),
                n => format!("{} {n}", chip.content),
            };
            match chip.own {
                true => (format!("[{text}]"), true),
                false => (text, false),
            }
        })
        .collect()
}

/// The row under a block: the chips muted, the reader's own in the text
/// colour, on the block's side; `None` without reactions.
pub fn row(chips: &[Chip], outbound: bool, width: usize, tokens: &Tokens) -> Option<RLine> {
    if chips.is_empty() {
        return None;
    }
    let words = words(chips);
    let text: Vec<&str> = words.iter().map(|(w, _)| w.as_str()).collect();
    let text = text.join("  ");
    let cols = fit_width(&text);
    let budget = width.saturating_sub(2);
    let mut line = RLine::default();
    if cols > budget {
        push(&mut line, " ", RStyle::default());
        push(
            &mut line,
            &truncate_to_cols(&text, budget),
            painted(tokens.muted),
        );
        return Some(line);
    }
    let pad = match outbound && width >= NARROW {
        true => width.saturating_sub(cols + 1),
        false => 1,
    };
    push(&mut line, &" ".repeat(pad), RStyle::default());
    for (i, (word, own)) in words.iter().enumerate() {
        if i > 0 {
            push(&mut line, "  ", RStyle::default());
        }
        let st = match own {
            true => RStyle::default(),
            false => painted(tokens.muted),
        };
        push(&mut line, word, st);
    }
    Some(line)
}

/// The list's preview of a reaction: `reacted 👍`, `you reacted 👍`.
pub fn preview(message: &Message) -> Option<String> {
    if !message.is_reaction() {
        return None;
    }
    let (_, content) = message.reaction()?;
    Some(match message.outbound {
        true => format!("you reacted {content}"),
        false => format!("reacted {content}"),
    })
}

/// The picker's row of quick reactions, each after its digit.
pub fn quick_row() -> String {
    let picks: Vec<String> = QUICK
        .iter()
        .enumerate()
        .map(|(i, q)| format!("{} {q}", i + 1))
        .collect();
    picks.join("  ")
}

impl<B> UiModel<B> {
    /// The picker's rows: what is reacted to, and the choices.
    pub(super) fn react_rows(&self, peer: [u8; 16], target: [u8; 32]) -> Vec<String> {
        let body = self
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .and_then(|c| c.messages.iter().find(|m| m.id == Some(target)))
            .map(|m| m.body.split('\n').next().unwrap_or("").to_string())
            .unwrap_or_default();
        vec![
            format!("react to: {body}"),
            quick_row(),
            "a digit, or type one and Enter".to_string(),
        ]
    }

    /// Send `content` as a reaction to `target` in `peer`'s conversation: a
    /// message of ours with the field and no text, delivered as any other.
    pub(super) fn react(
        &mut self,
        peer: [u8; 16],
        target: [u8; 32],
        content: &str,
    ) -> Vec<UiEffect> {
        let Some(content) = sanitise(content) else {
            return Vec::new();
        };
        let Some(index) = self.conversations.iter().position(|c| c.peer == peer) else {
            return Vec::new();
        };
        let field = Reaction {
            to: target,
            content: content.clone(),
        }
        .encode_field_value();
        let fields = vec![(FIELD_REACTION, field)];
        self.send_fields(index, String::new(), fields)
    }

    /// Move the cursor off a reaction, which has no block to stand on: to
    /// the nearest message before it that is drawn, or after it.
    pub(super) fn off_reactions(&mut self) {
        let Some(conversation) = self.conversations.get(self.selected) else {
            return;
        };
        let messages = &conversation.messages;
        let at = self.message_cursor;
        if !messages.get(at).is_some_and(Message::is_aside) {
            return;
        }
        let before = (0..at).rev().find(|&i| !messages[i].is_aside());
        let after = (at + 1..messages.len()).find(|&i| !messages[i].is_aside());
        if let Some(to) = before.or(after) {
            self.message_cursor = to;
        }
    }
}

impl<B: InputBuffer + Default> UiModel<B> {
    /// `e` or the palette's `react`: the picker over the message under the
    /// cursor.
    pub(super) fn ask_react(&mut self) {
        let Some(conversation) = self.conversations.get(self.selected) else {
            self.toast(ToastKind::Info, "no message here".to_string());
            return;
        };
        let peer = conversation.peer;
        let Some(message) = conversation.messages.get(self.message_cursor) else {
            self.toast(ToastKind::Info, "no message here".to_string());
            return;
        };
        let Some(target) = message.id else {
            self.toast(
                ToastKind::Info,
                "not sent yet: react once it has an id".to_string(),
            );
            return;
        };
        self.palette = Some(Palette {
            query: B::default(),
            selected: 0,
            ask: Some(Ask::React { peer, target }),
            drawings: false,
        });
    }
}
