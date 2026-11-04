//! Receipts for propagated post (1395,
//! `docs/src/concepts/loquax-propagation-receipts.md`).
//!
//! - Every message the model sends carries capability bit 2
//!   ([`leviculum_lxmf::lev::CAP_PROPAGATION_RECEIPTS`], set by
//!   [`super::groups::client_caps`]): "I acknowledge propagated post and want
//!   mine acknowledged".
//! - A message that came through the propagation network
//!   ([`crate::outbox::OutboxEvent::Propagated`], which the engine says after
//!   the router handed it on as `DeliveryMethod::Propagated`) and whose own
//!   mark carries bit 2 owes its sender a propagation receipt, key `0x07`
//!   ([`acknowledges`]). It gathers per sender with the clock of the first
//!   arrival and goes [`super::receipts::BATCH_MS`] after that arrival, alone
//!   or on the next message to that sender, as a read receipt does. It is not
//!   switchable and does not follow the read-receipts switch: it behaves like
//!   the transport proof. Read wins: a message seen before its batch goes,
//!   whose read receipt goes too, is left out, since read includes arrived.
//! - A client of a home node acknowledges nothing: the home node is the
//!   recipient's endpoint, collects from the propagation node and signs the
//!   receipt when the message reaches its archive
//!   (`crate::home::server::propagation_receipts`). The device takes the
//!   home's word for it as it takes a seen mark.
//! - A `0x07` for one of our messages moves it to the delivered rung, two
//!   grey ticks, whatever the read-receipts switch says, and the ledger keeps
//!   [`VIA_RECEIPT`]: the words in the detail view and for the screen reader
//!   say "delivered via the propagation network", where a proof says "on the
//!   device". Monotone: delivered or read stays as it is.
//! - What the propagation node says of a message (section B,
//!   [`crate::pn_status`]): `collected` is two grey ticks with
//!   [`COLLECTED`]; `expired` and `evicted` keep the one tick and add
//!   [`NOT_COLLECTED`], which never says lost, since with peering the
//!   recipient may have collected it at another node; `held` and `unknown`
//!   change nothing.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::lev::{Lev, PropagationReceipt};
use leviculum_lxmf::lev_status::MessageStatus;
use leviculum_ui::delivery::DeliveryState;

use super::{to_hex, Message, Trust, UiEffect, UiModel};
use crate::when::WallTime;

/// The ledger line of our message a propagation receipt named.
pub const VIA_RECEIPT: &str = "delivered via the propagation network";

/// The ledger line of our message the propagation node said its recipient
/// collected.
pub const COLLECTED: &str = "collected from the propagation network";

/// The ledger line of our message the propagation node no longer holds and
/// did not see collected: expired or displaced there. Never "lost": with
/// peering the recipient may have collected it at another node.
pub const NOT_COLLECTED: &str = "not collected at the propagation node; perhaps via another";

/// The ledger line of a received message once its propagation receipt was
/// owed, so a second copy owes none.
pub const ACKNOWLEDGED: &str = "arrival acknowledged to the sender";

/// What says a sent message got past the propagation node, as its ledger
/// keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Witness {
    /// The recipient's propagation receipt, `0x07`.
    Receipt,
    /// The propagation node saw the recipient collect it.
    Collected,
    /// The propagation node no longer holds it and did not see it
    /// collected.
    NotCollected,
}

impl Witness {
    /// The ledger line it is kept as.
    pub const fn ledger(self) -> &'static str {
        match self {
            Self::Receipt => VIA_RECEIPT,
            Self::Collected => COLLECTED,
            Self::NotCollected => NOT_COLLECTED,
        }
    }
}

/// What a sent message's ledger says got it past the propagation node, and
/// when this device learned it: the first of a receipt and a collection, or,
/// with neither, a node's word that it was not collected there. A
/// transport proof before them wins (the message is on the device, the
/// plain words); none for a message the propagation network never named.
pub fn witness(message: &Message) -> Option<(Witness, Option<WallTime>)> {
    if !message.outbound {
        return None;
    }
    let mut not_collected = None;
    for entry in &message.ledger {
        if entry
            .what
            .starts_with(super::ledger_words(DeliveryState::Delivered))
        {
            return None;
        }
        if entry.what == VIA_RECEIPT {
            return Some((Witness::Receipt, entry.at));
        }
        if entry.what == COLLECTED {
            return Some((Witness::Collected, entry.at));
        }
        if entry.what == NOT_COLLECTED && not_collected.is_none() {
            not_collected = Some((Witness::NotCollected, entry.at));
        }
    }
    not_collected
}

/// The acknowledgement rule: a message that came by propagation is
/// acknowledged when its own `lev` mark carries bit 2 and it is a message,
/// not a receipt of either kind (no receipt of a receipt) nor a group's copy
/// (the group's home strips bit 2, so this is belt and braces). A direct
/// one has its transport proof and gets none.
pub fn acknowledges(propagated: bool, lev: Option<&Lev>) -> bool {
    propagated
        && lev.is_some_and(|lev| {
            lev.supports_propagation_receipts()
                && lev.receipt.is_none()
                && lev.propagation_receipt.is_none()
                && lev.group_copy.is_none()
        })
}

/// The receipt alone to `peer`: the namespace with key `0x07` and no
/// capability mark, so it says nothing of read receipts either way; 18
/// bytes plus 34 an id.
pub fn alone(receipt: PropagationReceipt) -> Lev {
    Lev {
        propagation_receipt: Some(receipt),
        ..Lev::default()
    }
}

/// The receipts owed, per sender.
#[derive(Clone, Debug, Default)]
pub struct Arrivals {
    owed: BTreeMap<[u8; 16], Owed>,
}

impl Arrivals {
    /// The ids owed `peer` a propagation receipt, in arrival order.
    pub fn owed(&self, peer: &[u8; 16]) -> Option<&[[u8; 32]]> {
        self.owed.get(peer).map(|owed| owed.ids.as_slice())
    }
}

#[derive(Clone, Debug)]
struct Owed {
    /// The recipient's clock at the first arrival, unix seconds.
    received_at: u32,
    ids: Vec<[u8; 32]>,
    /// When the first arrived, on `elapsed_ms`.
    first_ms: u64,
}

impl<B> UiModel<B> {
    /// The engine says `message_id` from `source` came by propagation: owe
    /// its sender a receipt when the rule says so.
    pub(super) fn arrived_propagated(
        &mut self,
        source: [u8; 16],
        message_id: [u8; 32],
    ) -> Vec<UiEffect> {
        if self.home_client || self.names.trust(&source) == Trust::Blocked {
            return Vec::new();
        }
        let now = self.now;
        let elapsed = self.elapsed_ms;
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.peer == source) else {
            return Vec::new();
        };
        let Some(message) = conversation
            .messages
            .iter_mut()
            .find(|m| !m.outbound && m.id == Some(message_id))
        else {
            return Vec::new();
        };
        let lev = Lev::from_fields(&message.fields);
        if !acknowledges(true, lev.as_ref())
            || message
                .ledger
                .iter()
                .any(|entry| entry.what == ACKNOWLEDGED)
        {
            return Vec::new();
        }
        message.note(now, ACKNOWLEDGED);
        let activity = message.activity;
        self.changed.insert((source, activity));
        let received_at = now.map_or(0, |at| {
            u32::try_from(at.unix_secs.max(0)).unwrap_or(u32::MAX)
        });
        let owed = self.arrivals.owed.entry(source).or_insert(Owed {
            received_at,
            ids: Vec::new(),
            first_ms: elapsed,
        });
        if !owed.ids.contains(&message_id) {
            owed.ids.push(message_id);
        }
        Vec::new()
    }

    /// After every fold, beside [`UiModel::read_step`]: the propagation
    /// receipts that ride on this fold's sends to their peer, and those due
    /// alone.
    pub(super) fn arrival_step(&mut self, effects: &mut Vec<UiEffect>) {
        if self.arrivals.owed.is_empty() {
            return;
        }
        let peers: Vec<[u8; 16]> = self.arrivals.owed.keys().copied().collect();
        for peer in peers {
            self.drop_read(&peer);
        }
        let mut riding = Vec::new();
        for (e, effect) in effects.iter().enumerate() {
            if let UiEffect::Send { to, .. } = effect {
                if self.arrivals.owed.contains_key(to) && !riding.iter().any(|(_, p)| p == to) {
                    riding.push((e, *to));
                }
            }
        }
        for (e, peer) in riding {
            let Some(owed) = self.arrivals.owed.remove(&peer) else {
                continue;
            };
            let mut batches = PropagationReceipt::batches(owed.received_at, &owed.ids).into_iter();
            if let Some(UiEffect::Send { fields, .. }) = effects.get_mut(e) {
                if let Some(first) = batches.next() {
                    let mut lev = Lev::from_fields(fields).unwrap_or_default();
                    lev.capabilities
                        .get_or_insert(super::groups::client_caps(self.reading.on));
                    lev.propagation_receipt = Some(first);
                    lev.put_into(fields);
                }
            }
            let rest: Vec<[u8; 32]> = batches.flat_map(|r| r.ids).collect();
            if !rest.is_empty() {
                self.arrivals.owed.insert(
                    peer,
                    Owed {
                        ids: rest,
                        first_ms: 0,
                        ..owed
                    },
                );
            }
        }
        let now = self.elapsed_ms;
        let due: Vec<[u8; 16]> = self
            .arrivals
            .owed
            .iter()
            .filter(|(_, owed)| now >= owed.first_ms.saturating_add(super::receipts::BATCH_MS))
            .map(|(peer, _)| *peer)
            .collect();
        for peer in due {
            let Some(owed) = self.arrivals.owed.remove(&peer) else {
                continue;
            };
            for receipt in PropagationReceipt::batches(owed.received_at, &owed.ids) {
                effects.push(UiEffect::Log(format!(
                    "ARRIVAL receipt to={} ids={} received_at={}",
                    to_hex(&peer[..8]),
                    receipt.ids.len(),
                    receipt.received_at
                )));
                // A receipt of its own: the read receipt's slot, no bubble.
                self.sending.push_back((super::receipts::RECEIPT_SEND, 0));
                effects.push(UiEffect::Send {
                    to: peer,
                    body: String::new(),
                    fields: alone(receipt).fields().to_vec(),
                });
            }
        }
    }

    /// Read wins: an id owed `peer` that was seen while read receipts go to
    /// that peer is named by the read receipt, which includes arrived.
    fn drop_read(&mut self, peer: &[u8; 16]) {
        let reads = self.takes_receipts(peer);
        let Some(owed) = self.arrivals.owed.get_mut(peer) else {
            return;
        };
        if !reads {
            return;
        }
        let Some(conversation) = self.conversations.iter().find(|c| c.peer == *peer) else {
            return;
        };
        owed.ids.retain(|id| {
            !conversation.messages.iter().any(|m| {
                !m.outbound
                    && m.id == Some(*id)
                    && m.ledger
                        .iter()
                        .any(|entry| entry.what == super::receipts::SEEN)
            })
        });
        if owed.ids.is_empty() {
            self.arrivals.owed.remove(peer);
        }
    }

    /// A propagation receipt from `peer` named these ids: each of our
    /// messages to `peer` below delivered becomes delivered via the
    /// propagation network. Not switchable. The log lines of the ids
    /// dropped.
    pub(super) fn apply_arrival(
        &mut self,
        peer: [u8; 16],
        receipt: &PropagationReceipt,
    ) -> Vec<UiEffect> {
        let mut unknown = Vec::new();
        for id in &receipt.ids {
            if !self.witnessed(&peer, id, Witness::Receipt) {
                unknown.push(*id);
            }
        }
        unknown
            .iter()
            .map(|id| {
                UiEffect::Log(format!(
                    "ARRIVAL receipt dropped id={} from={}: no message of ours to that peer",
                    to_hex(&id[..8]),
                    to_hex(&peer[..8])
                ))
            })
            .collect()
    }

    /// What the propagation node answered of our messages, by message id
    /// (section B, [`crate::pn_status`]): the marks it moves. `held` and
    /// `unknown` move none.
    pub fn pn_answered(&mut self, answers: &[([u8; 32], MessageStatus)]) {
        for (id, status) in answers {
            let witness = match status {
                MessageStatus::Collected | MessageStatus::Delivered => Witness::Collected,
                MessageStatus::Expired | MessageStatus::Evicted => Witness::NotCollected,
                MessageStatus::Held | MessageStatus::Unknown => continue,
            };
            let peers: Vec<[u8; 16]> = self
                .conversations
                .iter()
                .filter(|c| c.messages.iter().any(|m| m.outbound && m.id == Some(*id)))
                .map(|c| c.peer)
                .collect();
            for peer in peers {
                self.witnessed(&peer, id, witness);
            }
        }
    }

    /// Our message `id` to `peer` has `witness`: the mark and the ledger
    /// line it moves. Whether there is such a message.
    fn witnessed(&mut self, peer: &[u8; 16], id: &[u8; 32], witness: Witness) -> bool {
        let now = self.now;
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.peer == *peer) else {
            return false;
        };
        let Some(message) = conversation
            .messages
            .iter_mut()
            .find(|m| m.outbound && m.id == Some(*id))
        else {
            return false;
        };
        let done = matches!(
            message.delivery,
            Some(DeliveryState::Delivered | DeliveryState::Read { .. })
        );
        let changed = match witness {
            Witness::Receipt | Witness::Collected if !done => {
                message.delivery = Some(DeliveryState::Delivered);
                message.note(now, witness.ledger());
                true
            }
            Witness::NotCollected
                if !done && !message.ledger.iter().any(|e| e.what == NOT_COLLECTED) =>
            {
                message.note(now, witness.ledger());
                true
            }
            _ => false,
        };
        if changed {
            self.changed.insert((*peer, message.activity));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_lxmf::lev::{
        GroupCopy, Receipt, CAP_GROUPS, CAP_PROPAGATION_RECEIPTS, CAP_READ_RECEIPTS,
    };

    fn caps(capabilities: u64) -> Lev {
        Lev {
            capabilities: Some(capabilities),
            ..Lev::default()
        }
    }

    /// The acknowledgement rule, case by case: propagated and bit 2 is a
    /// receipt; direct, bit unset, no mark, a receipt of either kind and a
    /// group's copy are none.
    #[test]
    fn the_acknowledgement_rule() {
        let asks = caps(CAP_PROPAGATION_RECEIPTS);
        assert!(acknowledges(true, Some(&asks)));
        assert!(acknowledges(
            true,
            Some(&caps(
                CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS
            ))
        ));
        assert!(!acknowledges(false, Some(&asks)), "direct has its proof");
        assert!(!acknowledges(
            true,
            Some(&caps(CAP_READ_RECEIPTS | CAP_GROUPS))
        ));
        assert!(!acknowledges(true, None), "a stock sender");
        let read = Lev {
            receipt: Some(Receipt {
                read_at: 1,
                ids: Vec::new(),
            }),
            ..asks.clone()
        };
        assert!(!acknowledges(true, Some(&read)), "no receipt of a receipt");
        let arrival = Lev {
            propagation_receipt: Some(PropagationReceipt {
                received_at: 1,
                ids: Vec::new(),
            }),
            ..asks.clone()
        };
        assert!(!acknowledges(true, Some(&arrival)));
        let copy = Lev {
            group_copy: Some(GroupCopy {
                sender: [1; 16],
                sent_ms: 1,
                epoch: 1,
            }),
            ..asks
        };
        assert!(!acknowledges(true, Some(&copy)));
    }

    /// The receipt alone is the spec's size: 18 bytes plus 34 an id in the
    /// custom-data field, no capability key.
    #[test]
    fn a_receipt_alone_carries_no_mark_and_costs_18_plus_34_an_id() {
        for n in 1..=3usize {
            let lev = alone(PropagationReceipt {
                received_at: 1_700_000_100,
                ids: alloc::vec![[7; 32]; n],
            });
            let [kind, data] = lev.fields();
            assert_eq!(lev.capabilities, None);
            // The two field keys, 0xFB and 0xFC, are two bytes each.
            assert_eq!(kind.1.len() + data.1.len() + 4, 18 + 34 * n, "{n} ids");
        }
    }
}
