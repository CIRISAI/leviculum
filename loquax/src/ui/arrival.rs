//! The recipient's device says it has our message: `lev` `0x07` (1395,
//! `docs/src/concepts/loquax-propagation-receipts.md`; since 1462 for
//! direct mail too, `docs/src/concepts/loquax-read-receipts.md`).
//!
//! - Every message the model sends carries capability bit 2
//!   ([`leviculum_lxmf::lev::CAP_PROPAGATION_RECEIPTS`], set by
//!   [`super::groups::client_caps`]): "I say when your post is on my
//!   device, and want mine said".
//! - A message on this device whose own mark carries bit 2 owes its sender
//!   a device receipt, key `0x07` ([`acknowledges`]), however it came:
//!   direct, by the propagation network, or pulled from this device's
//!   mailbox. The proof cannot say it (1462): a mailbox signs the proof with
//!   the recipient's identity as soon as it archived the message, so the
//!   writer could not tell its proof from a device's. One rule, no guess
//!   about who has a mailbox: a device without one sends it too. It gathers
//!   per sender with the clock of the first arrival and goes
//!   [`super::receipts::BATCH_MS`] after that arrival, alone or on the next
//!   message to that sender, as a read receipt does. It is not switchable
//!   and does not follow the read-receipts switch: it behaves like the
//!   transport proof. Read wins: a message seen before its batch goes,
//!   whose read receipt goes too, is left out, since read includes it.
//!   Position reports and a group's states owe none, nor does the history a
//!   new device's first pull hands it ([`super::UiModel::history`]): the
//!   device that took them live said so then.
//! - A client of a home node holds no identity, so it says it with a mark
//!   ([`crate::home::wire::MarkFlag::Arrived`]) and the home signs the
//!   receipt (`crate::home::server`), the first device's mark winning. The
//!   home itself acknowledges nothing: it is no device.
//! - A `0x07` for one of our messages moves it to the delivered rung, two
//!   grey ticks, whatever the read-receipts switch says, and the ledger keeps
//!   [`ON_DEVICE`]. Monotone: delivered or read stays as it is.
//! - What the propagation node says of a message (section B,
//!   [`crate::pn_status`]): `collected` is the arrived rung, one tick, with
//!   [`COLLECTED`]: the recipient's identity took it, which its mailbox may
//!   have done; `expired` and `evicted` keep the mark and add
//!   [`NOT_COLLECTED`], which never says lost, since with peering the
//!   recipient may have collected it at another node; `held` and `unknown`
//!   change nothing.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::lev::{Lev, PropagationReceipt};
use leviculum_lxmf::lev_status::MessageStatus;
use leviculum_lxmf::message::Field;
use leviculum_ui::delivery::DeliveryState;

use super::{to_hex, Message, Trust, UiEffect, UiModel};
use crate::when::WallTime;

/// The ledger line of our message the recipient's device receipt named
/// (1462).
pub const ON_DEVICE: &str = "on the recipient's device: its receipt";

/// The ledger line a propagation receipt left before 1462, when the
/// recipient's mailbox signed it too: a ledger kept from then still says
/// it, and it is no word of a device.
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
    /// The recipient's device receipt, `0x07`.
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
            Self::Receipt => ON_DEVICE,
            Self::Collected => COLLECTED,
            Self::NotCollected => NOT_COLLECTED,
        }
    }
}

/// What a sent message's ledger says got it on, and when this device
/// learned it: the first of a device receipt and a collection, or, with
/// neither, a node's word that it was not collected there. A transport
/// proof before them wins (the plain words of the rung); none for a
/// message nobody but its proof named.
pub fn witness(message: &Message) -> Option<(Witness, Option<WallTime>)> {
    if !message.outbound {
        return None;
    }
    let mut not_collected = None;
    for entry in &message.ledger {
        if is_proof(&entry.what) {
            return None;
        }
        if entry.what == ON_DEVICE || entry.what == VIA_RECEIPT {
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

/// Whether a ledger line is a transport proof's, today's or one kept from
/// before 1462 (an apply may have added the leg in brackets).
fn is_proof(what: &str) -> bool {
    what.starts_with(super::PROOF_LINE) || what.starts_with(super::PROOF_LINE_BEFORE_1462)
}

/// A message of ours kept from before 1462, when a proof, a propagation
/// receipt (which the recipient's mailbox signed too) or a collection was
/// two ticks: its ledger says which, and none of them is the device's word,
/// so it is shown at one tick now. A message whose two ticks came another
/// way (a group's status, an import) keeps them. The store applies it as it
/// loads a message; the next write keeps it.
pub fn before_1462(message: &mut Message) {
    if !message.outbound || message.delivery != Some(DeliveryState::Delivered) {
        return;
    }
    let ledger = &message.ledger;
    if ledger.iter().any(|entry| entry.what == ON_DEVICE) {
        return;
    }
    if ledger.iter().any(|entry| {
        entry.what.starts_with(super::PROOF_LINE_BEFORE_1462)
            || entry.what == VIA_RECEIPT
            || entry.what == COLLECTED
    }) {
        message.delivery = Some(DeliveryState::Arrived);
    }
}

/// When the ladder's rungs came (1462), as this device learned them: the
/// arrived rung (the first proof or collection) and the device's receipt.
/// The Info sheet names them beside Sent and Read.
pub fn rung_times(message: &Message) -> (Option<WallTime>, Option<WallTime>) {
    let first = |hit: &dyn Fn(&str) -> bool| {
        message
            .ledger
            .iter()
            .find(|entry| hit(&entry.what))
            .and_then(|entry| entry.at)
    };
    let arrived = first(&|what: &str| is_proof(what) || what == COLLECTED);
    let on_device = first(&|what: &str| what == ON_DEVICE);
    (arrived, on_device)
}

/// The acknowledgement rule (1462): a message on this device is
/// acknowledged, however it came, when its own `lev` mark carries bit 2 and
/// it is a message, not a bare receipt of either kind (no receipt of a
/// receipt). Bare is what the screen takes for a receipt alone and keeps
/// no bubble of (`receipts::alone`, 1557): no text, no field but
/// the namespace's pair. A message that carries one, an answer with the
/// read or device receipt riding on it (1486), is a message and is
/// acknowledged. A group's copy is one too since 1466: its home passes the
/// writer's bit 2 on, and the receipt goes to the group, which counts it
/// for the writer's two ticks. A group's status, membership or digest
/// carries no bit 2. A stock sender's message has no mark and gets none.
pub fn acknowledges(body: &[u8], fields: &[Field], lev: Option<&Lev>) -> bool {
    lev.is_some_and(|lev| {
        lev.supports_propagation_receipts() && !super::receipts::alone(body, fields, lev)
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
    /// `message_id` from `source` is on this device: owe its sender a
    /// device receipt when the rule says so ([`acknowledges`]). Said for
    /// every message that arrived, and again when the engine says one came
    /// by propagation; the ledger's [`ACKNOWLEDGED`] makes it once. History
    /// a first pull hands over owes none.
    pub(super) fn arrived_here(&mut self, source: [u8; 16], message_id: [u8; 32]) -> Vec<UiEffect> {
        if self.history || self.names.trust(&source) == Trust::Blocked {
            return Vec::new();
        }
        let now = self.now;
        let elapsed = self.elapsed_ms;
        let Some(conversation) = self.conversations.iter_mut().find(|c| c.peer == source) else {
            return Vec::new();
        };
        let Some(mut message) = conversation
            .messages
            .find_mut(|m| !m.outbound && m.id == Some(message_id))
        else {
            return Vec::new();
        };
        // The source's own mark (1482): a relay's copy gets no receipt.
        let lev = leviculum_lxmf::relay::own_lev(message.body.as_bytes(), &message.fields, &source);
        if !acknowledges(message.body.as_bytes(), &message.fields, lev.as_ref())
            || message.is_report()
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
        // A client of a home node signs nothing: the home does, on the
        // device's mark.
        if self.home_client {
            self.arrival_marks();
            return;
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

    /// On a client of a home node: the owed ids that are due, as marks the
    /// home node signs the receipts for (1462), at the clock of each
    /// sender's first arrival.
    fn arrival_marks(&mut self) {
        let now = self.elapsed_ms;
        let due: Vec<[u8; 16]> = self
            .arrivals
            .owed
            .iter()
            .filter(|(_, owed)| now >= owed.first_ms.saturating_add(super::receipts::BATCH_MS))
            .map(|(peer, _)| *peer)
            .collect();
        let mut by_time: BTreeMap<u32, Vec<[u8; 32]>> = BTreeMap::new();
        for peer in due {
            let Some(owed) = self.arrivals.owed.remove(&peer) else {
                continue;
            };
            by_time
                .entry(owed.received_at)
                .or_default()
                .extend(owed.ids);
        }
        for (at, ids) in by_time {
            self.mark_out(crate::home::wire::MarkFlag::Arrived { at }, ids);
        }
    }

    /// The home's archive says a device said these are on it (1462): owed
    /// no more here.
    pub(super) fn home_arrived(&mut self, ids: &[[u8; 32]]) {
        self.arrivals.owed.retain(|_, owed| {
            owed.ids.retain(|id| !ids.contains(id));
            !owed.ids.is_empty()
        });
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

    /// A device receipt from `peer` named these ids: each of our messages
    /// to `peer` below delivered becomes delivered, on the device (1462).
    /// Not switchable. The log lines of the ids dropped.
    pub(super) fn apply_arrival(
        &mut self,
        peer: [u8; 16],
        receipt: &PropagationReceipt,
    ) -> Vec<UiEffect> {
        let mut unknown = Vec::new();
        for id in &receipt.ids {
            // The recipient said it: the node is asked no more (1409).
            self.pn_asker.settled(id);
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
        let Some(mut message) = conversation
            .messages
            .find_mut(|m| m.outbound && m.id == Some(*id))
        else {
            return false;
        };
        let on_device = matches!(
            message.delivery,
            Some(DeliveryState::Delivered | DeliveryState::Read { .. })
        );
        let arrived = on_device || message.delivery == Some(DeliveryState::Arrived);
        let changed = match witness {
            Witness::Receipt if !on_device => {
                message.delivery = Some(DeliveryState::Delivered);
                message.note(now, witness.ledger());
                true
            }
            // The recipient's identity collected it, which its mailbox may
            // have done: one tick (1462).
            Witness::Collected if !arrived => {
                message.delivery = Some(DeliveryState::Arrived);
                message.note(now, witness.ledger());
                true
            }
            Witness::NotCollected
                if !arrived && !message.ledger.iter().any(|e| e.what == NOT_COLLECTED) =>
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
    use leviculum_lxmf::constants::{
        FIELD_AUDIO, FIELD_FILE_ATTACHMENTS, FIELD_IMAGE, FIELD_REACTION,
    };
    use leviculum_lxmf::lev::{
        GroupCopy, Receipt, CAP_GROUPS, CAP_PROPAGATION_RECEIPTS, CAP_READ_RECEIPTS,
    };

    fn caps(capabilities: u64) -> Lev {
        Lev {
            capabilities: Some(capabilities),
            ..Lev::default()
        }
    }

    /// The acknowledgement rule, case by case: bit 2 is a receipt, a
    /// group's copy with it too (1466), a text or attachment that carries a
    /// receipt too (1486); bit unset, no mark, a bare receipt of either kind
    /// are none, and a group's copy without bit 2 (its writer
    /// did not ask) is none.
    #[test]
    fn the_acknowledgement_rule() {
        let asks = caps(CAP_PROPAGATION_RECEIPTS);
        assert!(acknowledges(b"text", &[], Some(&asks)));
        assert!(acknowledges(
            b"text",
            &[],
            Some(&caps(
                CAP_READ_RECEIPTS | CAP_GROUPS | CAP_PROPAGATION_RECEIPTS
            ))
        ));
        assert!(!acknowledges(
            b"text",
            &[],
            Some(&caps(CAP_READ_RECEIPTS | CAP_GROUPS))
        ));
        assert!(!acknowledges(b"text", &[], None), "a stock sender");
        let read = Lev {
            receipt: Some(Receipt {
                read_at: 1,
                ids: Vec::new(),
            }),
            ..asks.clone()
        };
        assert!(
            !acknowledges(b"", &[], Some(&read)),
            "no receipt of a receipt"
        );
        assert!(!acknowledges(b" \n", &[], Some(&read)), "blank is bare");
        assert!(
            acknowledges(b"text", &[], Some(&read)),
            "1486: an answer that carries a read receipt"
        );
        let arrival = Lev {
            propagation_receipt: Some(PropagationReceipt {
                received_at: 1,
                ids: Vec::new(),
            }),
            ..asks.clone()
        };
        assert!(!acknowledges(b"", &[], Some(&arrival)));
        assert!(
            acknowledges(b"text", &[], Some(&arrival)),
            "1486: an answer that carries a device receipt"
        );
        for key in [FIELD_FILE_ATTACHMENTS, FIELD_IMAGE, FIELD_AUDIO] {
            assert!(
                acknowledges(b"", &[(key, vec![0xc0])], Some(&arrival)),
                "1486: an attachment {key} that carries one"
            );
        }
        // 1557: bare is what the screen takes for a receipt alone
        // (`receipts::alone`): any field beside the namespace's pair makes
        // a message the screen keeps, and what it keeps it acknowledges.
        let rider = arrival.fields().to_vec();
        assert!(
            !acknowledges(b"", &rider, Some(&arrival)),
            "a receipt with its own namespace pair only is bare"
        );
        let mut reaction = rider.clone();
        reaction.push((FIELD_REACTION, vec![0xc0]));
        assert!(
            acknowledges(b"", &reaction, Some(&arrival)),
            "1557: a field that is no attachment is still a message"
        );
        let copy = Lev {
            group_copy: Some(GroupCopy {
                sender: [1; 16],
                sent_ms: 1,
                epoch: 1,
            }),
            ..asks
        };
        assert!(
            acknowledges(b"text", &[], Some(&copy)),
            "1466: a group's copy"
        );
        let unasked = Lev {
            capabilities: Some(CAP_GROUPS),
            ..copy
        };
        assert!(!acknowledges(b"text", &[], Some(&unasked)));
    }

    /// 1462: a stored message at two ticks loads at one when its ledger
    /// shows only a proof, a mailbox-signed propagation receipt or a
    /// collection; the device's receipt, a group's status, an incoming
    /// message and another rung keep what they had.
    #[test]
    fn a_message_kept_from_before_1462_is_one_tick_unless_its_device_said_so() {
        use crate::ui::{LedgerEntry, Signature, PROOF_LINE_BEFORE_1462};
        let message = |outbound: bool, delivery: DeliveryState, lines: &[&str]| Message {
            outbound,
            size: 2,
            body: "hi".into(),
            id: Some([1; 32]),
            delivery: Some(delivery),
            signature: Signature::Verified,
            leg: None,
            unproven: false,
            at: None,
            ledger: lines
                .iter()
                .map(|line| LedgerEntry::new(None, line))
                .collect(),
            last_leg: None,
            activity: 1,
            fields: Vec::new(),
        };
        let after = |mut m: Message| {
            before_1462(&mut m);
            m.delivery
        };
        let d = DeliveryState::Delivered;
        let proof = "delivered: proof received (direct)";
        assert!(proof.starts_with(PROOF_LINE_BEFORE_1462));
        for lines in [&[proof][..], &[VIA_RECEIPT], &[COLLECTED]] {
            assert_eq!(
                after(message(true, d, lines)),
                Some(DeliveryState::Arrived),
                "{lines:?}"
            );
        }
        assert_eq!(after(message(true, d, &[proof, ON_DEVICE])), Some(d));
        assert_eq!(
            after(message(true, d, &["the group has it: proof received"])),
            Some(d)
        );
        assert_eq!(after(message(false, d, &[proof])), Some(d));
        let read = DeliveryState::Read { at: 5 };
        assert_eq!(after(message(true, read, &[proof])), Some(read));
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
