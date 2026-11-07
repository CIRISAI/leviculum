//! The bench's arrivals (order 1540): a message that comes into a chat
//! with no network and no mailbox, so a walk can open an unread chat as
//! often as it likes, which is Lew's case of 2026-11-07 (an unread chat,
//! Enter, the read mark and the rows written while the UI core folds).
//! Only the M9's bench image calls this (`TYPE_BENCH_ARRIVE`,
//! `leviculum-esp`'s `cfg(m9_bench)`); the host tests pin it.

use alloc::format;
use alloc::vec::Vec;

use leviculum_lxmf::msgpack;
use loquax::home::wire::Entry;
use loquax::home::KIND_RECEIVED;
use loquax::ui::UiModel;

use crate::field::Field;

/// The time an arrival is written at when no chat says one: 2026-11-01.
const NO_TIME: i64 = 1_793_500_000;

/// Where an arrival goes: the chat at `row` of the list as shown, counted
/// modulo the rows there are, so a walk cycles through every chat
/// without knowing how many there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    /// The row it went to, after the modulo.
    pub row: usize,
    /// The rows there are.
    pub of: usize,
    /// The chat's index among the model's conversations.
    pub index: usize,
    /// The chat's peer, the message's source.
    pub peer: [u8; 16],
    /// When it is written, unix seconds: a second after the newest chat,
    /// so it is the newest message and its chat goes to the list's top.
    pub at: i64,
}

/// Where an arrival at `row` goes in `model`; `None` when the list is
/// empty.
pub fn arrival(model: &UiModel<Field>, row: u8) -> Option<Arrival> {
    let order = model.order();
    let of = order.len();
    let row = usize::from(row).checked_rem(of)?;
    let index = *order.get(row)?;
    let peer = model.conversations.get(index)?.peer;
    let at = model
        .conversations
        .iter()
        .filter_map(|c| c.last_written())
        .max()
        .unwrap_or(NO_TIME)
        + 1;
    Some(Arrival {
        row,
        of,
        index,
        peer,
        at,
    })
}

/// The archive entry of an arrival from `peer` to the home at `address`,
/// the `n`th since boot (its words say which, so every arrival is its own
/// message): a received message as the home keeps it, in LXMF's packed
/// form, its signature zero. The board never checks a received message's
/// signature (`loquax::home::board::fold` has no sender's identity), so
/// the fold takes it as a pulled one. `seq` is the store's cursor: the
/// entry moves nothing the home's sequences say.
pub fn entry(peer: [u8; 16], address: [u8; 16], at: i64, n: u32, seq: u64) -> Entry {
    let words = format!("bench arrival {n}");
    let mut packed = Vec::with_capacity(128);
    packed.extend_from_slice(&address);
    packed.extend_from_slice(&peer);
    packed.extend_from_slice(&[0u8; 64]);
    msgpack::array(&mut packed, 4);
    msgpack::f64(&mut packed, at as f64);
    msgpack::bin(&mut packed, b"");
    msgpack::bin(&mut packed, words.as_bytes());
    msgpack::map(&mut packed, 0);
    Entry {
        seq,
        kind: KIND_RECEIVED,
        at,
        packed,
    }
}
