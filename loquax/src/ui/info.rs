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
    let who = model.names.label(&conversation.peer, model.lang);
    let name = || Arg::Str(who.clone());
    let group = model.groups.contains_key(&conversation.peer);
    let mut rows = Vec::new();
    // A relayed post (1482) is from its writer, via the relay: the
    // writer's name, its address's 4-hex tail (1452: here and nowhere
    // else on screen) and the relay's name.
    let relayed = (!message.outbound && !group && super::relayed::is_relayed(conversation))
        .then(|| super::relayed::writer_of(message, &conversation.peer))
        .flatten();
    rows.push(match (message.outbound, relayed) {
        (true, _) => InfoRow::new(StrId::InfoTo, Text::Raw(who.clone())),
        (false, Some(writer)) => InfoRow::new(
            StrId::InfoFrom,
            Text::Key(
                StrId::InfoRelayedFrom,
                Vec::from([
                    Arg::Str(model.relayed_name(&writer)),
                    Arg::Str(crate::names::hash_suffix(&writer.address)),
                    Arg::Str(who.clone()),
                ]),
            ),
        ),
        (false, None) => InfoRow::new(StrId::InfoFrom, Text::Raw(who.clone())),
    });
    // Sent is when it was written, ours or theirs; Received is when this
    // device took it, the first line of its ledger (1445): a history pulled
    // at once shows its writers' days and says when it came here.
    if let Some(at) = message.at {
        rows.push(InfoRow::new(StrId::InfoSent, when(&at, model.now.as_ref())));
    }
    // A shared position's coordinates and accuracy live here, not in the
    // bubble, which says one sentence (1449 item 5, walk 1 finding 11).
    if let Some(fix) = super::message_fix(conversation.peer, message) {
        rows.push(InfoRow::new(
            StrId::InfoPlace,
            Text::Raw(crate::telemetry::coordinates(&fix.location)),
        ));
        if let Some(accuracy) = crate::telemetry::accuracy(&fix.location) {
            rows.push(InfoRow::new(StrId::InfoAccuracy, Text::Raw(accuracy)));
        }
    }
    // The ladder's times of ours (1462): when its proof came (the
    // recipient's mailbox or device), when the device said it has it, when
    // it was read; each once known, read only where receipts are shown.
    if message.outbound {
        let (arrived, on_device) = super::arrival::rung_times(message);
        let read = match message
            .delivery
            .map(|state| shown_delivery(state, model.home_client, model.reading.on))
        {
            Some(DeliveryState::Read { at }) if at > 0 => Some(WallTime {
                unix_secs: i64::from(at),
                offset_secs: message.at.or(model.now).map_or(0, |n| n.offset_secs),
            }),
            _ => None,
        };
        // In a group each member has its own rungs (1466), its rows below.
        let (arrived, on_device) = match group {
            true => (None, None),
            false => (arrived, on_device),
        };
        for (label, at) in [
            (StrId::InfoArrived, arrived),
            (StrId::InfoOnDevice, on_device),
            (StrId::InfoReadAt, read),
        ] {
            if let Some(at) = at {
                rows.push(InfoRow::new(label, when(&at, model.now.as_ref())));
            }
        }
    }
    let taken = message.ledger.first().and_then(|entry| entry.at);
    if let (false, Some(at)) = (message.outbound, taken) {
        rows.push(InfoRow::new(
            StrId::InfoReceived,
            when(&at, model.now.as_ref()),
        ));
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
                DeliveryState::Arrived => match witness {
                    Some((Witness::Collected, at)) => {
                        Text::Key(StrId::InfoCollected, Vec::from([hh_mm(at)]))
                    }
                    _ => Text::key(StrId::StateArrived),
                },
                DeliveryState::Delivered if group => Text::key(StrId::StateGroupDelivered),
                DeliveryState::Delivered => Text::Key(StrId::InfoDelivered, Vec::from([name()])),
                // Read at no known time (1506): "Read", never the epoch.
                DeliveryState::Read { at: 0 } => Text::key(StrId::StateReadUntimed),
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
            if message.outbound
                && matches!(state, DeliveryState::Arrived | DeliveryState::Delivered)
            {
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

impl<B> UiModel<B> {
    /// [`super::UiEvent::Info`]: the message `key` of `peer`'s chat in the
    /// detail view, the terminal's `details` on it (1457); `None`, or a
    /// message not there, closes it.
    pub(super) fn show_info(&mut self, at: Option<([u8; 16], u64)>) -> Vec<super::UiEffect> {
        self.detail = at.and_then(|(peer, key)| {
            let c = self.conversations.iter().position(|c| c.peer == peer)?;
            let m = self.conversations[c]
                .messages
                .iter()
                .position(|m| m.activity == key)?;
            Some((c, m))
        });
        if self.detail.is_some() {
            self.profile_open = false;
        }
        self.detail_scroll = 0;
        Vec::new()
    }

    /// The Info of the message whose detail view is up, as the window
    /// draws it (1457); none while none is.
    pub fn info_shown(&self) -> Option<MessageInfo> {
        let (c, m) = self.detail?;
        let conversation = self.conversations.get(c)?;
        let key = conversation.messages.get(m)?.activity;
        message_info(self, conversation.peer, key)
    }
}

/// The group's counts of our message `id`, and since 1466 a row per member
/// with its rung and since when (proof, device, read): none where `peer` is
/// no group.
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
    let offset = model.now.map_or(0, |n| n.offset_secs);
    for (name, rung) in model.group_rungs(peer, id) {
        let at = |t: u32| WallTime {
            unix_secs: i64::from(t),
            offset_secs: offset,
        };
        let value = match (rung.read_at, rung.on_device_at, rung.proven_at) {
            (r, _, _) if r > 0 => Text::Join(Vec::from([
                Text::key(StrId::InfoReadAt),
                when(&at(r), model.now.as_ref()),
            ])),
            (_, d, _) if d > 0 => Text::Join(Vec::from([
                Text::key(StrId::InfoOnDevice),
                when(&at(d), model.now.as_ref()),
            ])),
            (_, _, p) if p > 0 => Text::Join(Vec::from([
                Text::key(StrId::InfoArrived),
                when(&at(p), model.now.as_ref()),
            ])),
            _ => Text::key(StrId::InfoMemberWaiting),
        };
        rows.push(InfoRow {
            label: Text::Raw(name),
            value,
        });
    }
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
