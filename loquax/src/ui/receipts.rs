//! Read receipts, slice one (1230): the wire and the Read state.
//!
//! `docs/src/concepts/loquax-read-receipts.md` is the design. What this
//! slice holds:
//!
//! - Every message the model sends carries the `lev` namespace's capability
//!   mark ([`leviculum_lxmf::lev::Lev::mark`]): "I send read receipts and
//!   want them". The mark rides in the custom field pair, which every
//!   client checked ignores.
//! - Every message received says whether its sender carries the mark, and
//!   the latest one decides the conversation's
//!   [`super::history::Flags::receipts`]: a peer that turned receipts off,
//!   or a client without them, stops being sent any. A receipt proves
//!   support too, whatever the message before it said.
//! - A receipt names our messages by their LXMF id; each one in the
//!   receipt's sender's conversation becomes [`DeliveryState::Read`], at the
//!   reader's time kept within when it was sent and when the receipt came.
//!   Read is monotone: a second receipt and a late proof leave it. An id the
//!   conversation does not hold as ours to that peer is dropped with a log
//!   line: nobody confirms a message that was not sent to them.
//! - A receipt alone, an empty message with nothing but the namespace, is a
//!   state and not a message, as a position report is (1209): no bubble, no
//!   unread, no toast, no move in the list, no conversation made for it.
//!
//! Sending receipts, and deciding what was read, is the next slice.

use alloc::format;
use alloc::vec::Vec;

use leviculum_lxmf::constants::{FIELD_CUSTOM_DATA, FIELD_CUSTOM_TYPE};
use leviculum_lxmf::lev::{Lev, Receipt};
use leviculum_lxmf::Field;
use leviculum_ui::delivery::DeliveryState;

use super::{to_hex, UiEffect, UiModel};

/// Put the capability mark into every send among `effects`.
pub(super) fn mark_sends(effects: &mut [UiEffect]) {
    for effect in effects {
        if let UiEffect::Send { fields, .. } = effect {
            Lev::mark().put_into(fields);
        }
    }
}

/// Whether a received message is a receipt and nothing else: no text, no
/// field but the namespace's pair.
pub(super) fn alone(body: &[u8], fields: &[Field], lev: &Lev) -> bool {
    lev.receipt.is_some()
        && body.iter().all(u8::is_ascii_whitespace)
        && fields
            .iter()
            .all(|(key, _)| *key == FIELD_CUSTOM_TYPE || *key == FIELD_CUSTOM_DATA)
}

/// Whether a message's namespace says its sender sends and wants receipts.
fn supports(lev: Option<&Lev>) -> bool {
    lev.is_some_and(|lev| lev.supports_receipts() || lev.receipt.is_some())
}

impl<B> UiModel<B> {
    /// What a message from `source` with this namespace says: the peer's
    /// mark, and the receipt it carries. The log lines of the ids dropped.
    pub(super) fn take_lev(&mut self, source: [u8; 16], lev: Option<&Lev>) -> Vec<UiEffect> {
        let Some(index) = self.conversations.iter().position(|c| c.peer == source) else {
            return lev
                .and_then(|lev| lev.receipt.as_ref())
                .map(|receipt| dropped(source, &receipt.ids))
                .unwrap_or_default();
        };
        let conversation = &mut self.conversations[index];
        let supported = supports(lev);
        if conversation.flags.receipts != supported {
            conversation.flags.receipts = supported;
            self.changed_peers.insert(source);
        }
        match lev.and_then(|lev| lev.receipt.as_ref()) {
            Some(receipt) => self.apply_receipt(index, receipt),
            None => Vec::new(),
        }
    }

    fn apply_receipt(&mut self, index: usize, receipt: &Receipt) -> Vec<UiEffect> {
        let now = self.now;
        let conversation = &mut self.conversations[index];
        let peer = conversation.peer;
        let mut unknown = Vec::new();
        for id in &receipt.ids {
            let Some(message) = conversation
                .messages
                .iter_mut()
                .find(|m| m.outbound && m.id == Some(*id))
            else {
                unknown.push(*id);
                continue;
            };
            if matches!(message.delivery, Some(DeliveryState::Read { .. })) {
                continue;
            }
            let sent = message.at.map(|at| at.unix_secs);
            let came = now.map(|at| at.unix_secs);
            message.delivery = Some(DeliveryState::Read {
                at: kept_within(receipt.read_at, sent, came),
            });
            message.note(now, "read by the peer");
            self.changed.insert((peer, message.activity));
        }
        dropped(peer, &unknown)
    }
}

/// The reader's clock, which may be wrong, kept within when the message
/// was sent and when the receipt came, where this device knows them.
fn kept_within(read_at: u32, sent: Option<i64>, came: Option<i64>) -> u32 {
    let mut at = i64::from(read_at);
    if let Some(came) = came {
        at = at.min(came);
    }
    if let Some(sent) = sent {
        at = at.max(sent);
    }
    u32::try_from(at.max(0)).unwrap_or(u32::MAX)
}

fn dropped(peer: [u8; 16], ids: &[[u8; 32]]) -> Vec<UiEffect> {
    ids.iter()
        .map(|id| {
            UiEffect::Log(format!(
                "RECEIPT dropped id={} from={}: no message of ours to that peer",
                to_hex(&id[..8]),
                to_hex(&peer[..8])
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readers_clock_is_kept_within_sent_and_came() {
        assert_eq!(kept_within(150, Some(100), Some(200)), 150);
        assert_eq!(kept_within(50, Some(100), Some(200)), 100);
        assert_eq!(kept_within(250, Some(100), Some(200)), 200);
        assert_eq!(kept_within(250, None, None), 250);
    }
}
