//! A message's Info, what the phone's long press opens (1381, finding 19
//! of the second ninety percent walk): plain rows in the reader's
//! language, a label and its value each, from the catalogue. Who, when,
//! where it stands, the group's counts, the size in KB; no hex. The
//! terminal's detail lines (the peer, the id, the device's log) come
//! along as [`MessageInfo::advanced`], which the phone shows only when the
//! reader opens Advanced.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::delivery::DeliveryState;

use super::arrival::{witness, Witness};
use super::strings::{Arg, Lang, StrId, Text};
use super::{detail_view, shown_delivery, Signature, UiModel};
use crate::when::WallTime;

/// One row of the Info: what it is and what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoRow {
    pub label: Text,
    pub value: Text,
}

impl InfoRow {
    fn new(label: StrId, value: Text) -> Self {
        Self {
            label: Text::key(label),
            value,
        }
    }

    /// The label and the value in `lang`.
    pub fn resolve(&self, lang: Lang) -> (String, String) {
        (self.label.resolve(lang), self.value.resolve(lang))
    }
}

/// A message's Info: its plain rows, and the terminal's detail lines for
/// Advanced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageInfo {
    pub rows: Vec<InfoRow>,
    pub advanced: Vec<String>,
}

/// The Info of the message `key` (its [`super::Message::activity`]) in
/// `peer`'s conversation; none when there is no such message.
pub fn message_info<B>(model: &UiModel<B>, peer: [u8; 16], key: u64) -> Option<MessageInfo> {
    let c = model.conversations.iter().position(|c| c.peer == peer)?;
    let conversation = &model.conversations[c];
    let m = conversation
        .messages
        .iter()
        .position(|m| m.activity == key)?;
    let message = &conversation.messages[m];
    let who = model.names.label(&conversation.peer);
    let name = || Arg::Str(who.clone());
    let mut rows = Vec::new();
    rows.push(match message.outbound {
        true => InfoRow::new(StrId::InfoTo, Text::Raw(who.clone())),
        false => InfoRow::new(StrId::InfoFrom, Text::Raw(who.clone())),
    });
    if let Some(at) = message.at {
        let label = match message.outbound {
            true => StrId::InfoSent,
            false => StrId::InfoReceived,
        };
        rows.push(InfoRow::new(label, when(&at, model.now.as_ref())));
    }
    match (message.delivery, message.signature) {
        (Some(state), _) => {
            let state = shown_delivery(state, model.home_client, model.reading.on);
            // What the propagation network said of it (1395): the words
            // beside the ticks, which are the proof's.
            let witness = witness(message);
            let not_collected = matches!(witness, Some((Witness::NotCollected, _)));
            let hh_mm =
                |at: Option<WallTime>| Arg::Str(at.map(|at| at.hh_mm()).unwrap_or_default());
            let value = match state {
                DeliveryState::Queued => Text::key(StrId::InfoWaiting),
                DeliveryState::Stamping => Text::key(StrId::InfoPreparing),
                DeliveryState::InFlight => Text::key(StrId::InfoSending),
                DeliveryState::Sent | DeliveryState::InMailbox if not_collected => {
                    Text::key(StrId::InfoNotCollected)
                }
                DeliveryState::Sent => Text::key(StrId::StateUnconfirmed),
                DeliveryState::InMailbox => Text::Key(StrId::InfoMailbox, Vec::from([name()])),
                DeliveryState::Delivered => match witness {
                    Some((Witness::Receipt, at)) => {
                        Text::Key(StrId::InfoDeliveredViaPn, Vec::from([hh_mm(at)]))
                    }
                    Some((Witness::Collected, at)) => {
                        Text::Key(StrId::InfoCollected, Vec::from([hh_mm(at)]))
                    }
                    _ => Text::Key(StrId::InfoDelivered, Vec::from([name()])),
                },
                DeliveryState::Read { at } => {
                    let at = WallTime {
                        unix_secs: i64::from(at),
                        offset_secs: message.at.or(model.now).map_or(0, |n| n.offset_secs),
                    };
                    Text::Key(StrId::StateRead, Vec::from([Arg::Str(at.hh_mm())]))
                }
                DeliveryState::Rejected => Text::key(StrId::InfoRefused),
                DeliveryState::Failed => Text::key(StrId::StateFailed),
            };
            rows.push(InfoRow::new(StrId::InfoState, value));
            if message.outbound && state == DeliveryState::Delivered {
                let why = match (model.reading.on, conversation.flags.receipts) {
                    (false, _) => Some(Text::key(StrId::InfoReceiptsOff)),
                    (true, false) => Some(Text::Key(StrId::InfoNoReceipts, Vec::from([name()]))),
                    (true, true) => None,
                };
                rows.extend(why.map(|why| InfoRow::new(StrId::InfoRead, why)));
            }
        }
        (None, signature) => {
            let value = match signature {
                Signature::Verified => Text::key(StrId::InfoSignatureOk),
                Signature::Unverified => Text::key(StrId::InfoSignatureUnchecked),
                Signature::Invalid => Text::Key(StrId::InfoSignatureBad, Vec::from([name()])),
            };
            rows.push(InfoRow::new(StrId::InfoSender, value));
        }
    }
    if let (true, Some(id)) = (message.outbound, message.id) {
        rows.extend(group_rows(model, &conversation.peer, &id));
    }
    rows.push(InfoRow::new(StrId::InfoSize, kilobytes(message.size)));
    Some(MessageInfo {
        rows,
        advanced: detail_view(model, c, m)?.rows,
    })
}

/// The group's counts of our message `id`: none where `peer` is no group.
fn group_rows<B>(model: &UiModel<B>, peer: &[u8; 16], id: &[u8; 32]) -> Vec<InfoRow> {
    if !model.groups.contains_key(peer) {
        return Vec::new();
    }
    let Some(status) = model.group_status(peer, id) else {
        return Vec::from([InfoRow::new(
            StrId::InfoGroup,
            Text::key(StrId::InfoGroupNone),
        )]);
    };
    let count = |n: u64| Arg::Int(i64::try_from(n).unwrap_or(i64::MAX));
    let mut rows = Vec::from([InfoRow::new(
        StrId::InfoGroup,
        Text::Key(
            StrId::InfoGroupDelivered,
            Vec::from([count(status.delivered), count(status.members)]),
        ),
    )]);
    let read = match status.readers {
        0 => Text::key(StrId::InfoGroupNoReaders),
        readers => {
            let read = Text::Key(
                StrId::InfoGroupRead,
                Vec::from([count(status.read), count(readers)]),
            );
            match status.members.saturating_sub(readers) {
                0 => read,
                quiet => Text::Join(Vec::from([
                    read,
                    Text::Key(StrId::InfoGroupQuiet, Vec::from([count(quiet)])),
                ])),
            }
        }
    };
    rows.push(InfoRow::new(StrId::InfoRead, read));
    rows
}

/// When it was sent or came: the day line's words and the time,
/// `Today · 14:05`.
fn when(at: &WallTime, now: Option<&WallTime>) -> Text {
    Text::Join(Vec::from([
        super::scene::day_words(at, now),
        Text::Raw(at.hh_mm()),
    ]))
}

/// `bytes` in KB with one decimal, rounded up so a short message is no
/// `0.0 KB`: `0.1 KB`, `2.4 KB`.
fn kilobytes(bytes: usize) -> Text {
    let tenths = bytes.saturating_mul(10).div_ceil(1024).max(1);
    let tenths = i64::try_from(tenths).unwrap_or(i64::MAX);
    Text::Key(
        StrId::InfoKb,
        Vec::from([Arg::Int(tenths / 10), Arg::Int(tenths % 10)]),
    )
}
