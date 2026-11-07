//! The M9 as a device of its home, on the model's side (1415): the model a
//! board that joined a home starts from, the archive's pages folded into
//! it (`loquax::home::board::fold`), and the sends the model asks for
//! turned into the home's `/send`. The board's loop
//! (`leviculum_esp::handheld`) performs the asks; this is what it hands
//! the model and takes from it, `no_std` and tested on the host.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use loquax::home::board;
use loquax::home::board::store::{self, Appended, Restored, Sectors, Store};
use loquax::home::wire::{Entry, Submission};
use loquax::ui::{update, Pane, UiEffect, UiEvent, UiModel};

use crate::field::Field;

/// The model of a board that is a device of the home whose LXMF address
/// is `address`: the home's identity is the board's, so what the home took
/// in is the board's own post. Empty until the first pull.
pub fn model(address: [u8; 16]) -> UiModel<Field> {
    let mut model = UiModel::<Field>::new(&[]);
    model.lang = crate::LANG;
    model.address = Some(address);
    model.focus = Pane::Conversations;
    // The board's map, as the fixture has it (1476): without it the map
    // a home board opens is sized for a terminal, not the picture.
    crate::map::ready(&mut model);
    model
}

/// Fold a page's entries into `model`; how many it took. Whatever the
/// model asks back (a receipt, a mark) is dropped: the board answers
/// nothing on its own yet.
pub fn take(model: &mut UiModel<Field>, entries: &[Entry], address: &[u8; 16]) -> usize {
    let mut taken = 0;
    for entry in entries {
        if let Some(event) = board::fold(entry, address) {
            update(model, UiEvent::Engine(event));
            taken += 1;
        }
    }
    taken
}

/// What a board that is a device of the home at `address` shows at boot
/// (1476): its model with every entry its message store on `dev` holds
/// folded in, the store, and what the store found. The client's cursor
/// then agrees with [`Store::cursor`] (`board::agree`).
pub fn restore<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
) -> Result<(UiModel<Field>, Store, Restored), S::Error> {
    let mut shown = model(address);
    let (store, restored) = Store::open(dev, store::tag(&address), |entry| {
        take(&mut shown, core::slice::from_ref(&entry), &address);
    })?;
    Ok((shown, store, restored))
}

/// Keep a pulled page in `store`: the entries the board's fold keeps
/// (`board::fold`, what the list and a chat show), and the page's newest
/// sequence as the store's end, in one write per sector it touches. An
/// empty page writes nothing.
pub fn keep<S: Sectors>(
    store: &mut Store,
    dev: &mut S,
    entries: &[Entry],
    address: &[u8; 16],
) -> Result<Appended, S::Error> {
    let Some(last) = entries.iter().map(|entry| entry.seq).max() else {
        return Ok(Appended::default());
    };
    let kept: Vec<Entry> = entries
        .iter()
        .filter(|entry| board::fold(entry, address).is_some())
        .cloned()
        .collect();
    store.append(dev, &kept, last)
}

/// The sends among `effects`, as the home takes them, each with an id of
/// its own from `ids`: the same id asked again is the same message at the
/// home, so a send asked twice after a lost answer goes once.
pub fn sends(effects: Vec<UiEffect>, ids: &mut impl FnMut() -> [u8; 16]) -> Vec<Submission> {
    effects
        .into_iter()
        .filter_map(|effect| match effect {
            UiEffect::Send { to, body, fields } => Some(Submission {
                client_id: ids(),
                destination: to,
                title: Vec::new(),
                content: body.into_bytes(),
                fields,
            }),
            _ => None,
        })
        .collect()
}

/// The sends waiting for the home, oldest first, asked one at a time.
#[derive(Debug, Default)]
pub struct Outgoing {
    queue: VecDeque<Submission>,
}

impl Outgoing {
    pub fn push(&mut self, submissions: Vec<Submission>) {
        self.queue.extend(submissions);
    }

    /// The oldest, still queued until [`Outgoing::done`].
    pub fn next(&self) -> Option<&Submission> {
        self.queue.front()
    }

    /// The oldest was asked and is over: taken or given up.
    pub fn done(&mut self) -> Option<Submission> {
        self.queue.pop_front()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// Tell `model` its oldest send ended: taken by the home under the
/// message's id, which names the bubble as the desktop's runner names it
/// (`Queued`, so the sent entry the next pull brings finds it and draws no
/// second one), or why not.
pub fn sent(model: &mut UiModel<Field>, result: Result<[u8; 32], alloc::string::String>) {
    match result {
        Ok(message_id) => {
            update(
                model,
                UiEvent::Engine(loquax::outbox::OutboxEvent::Queued { message_id }),
            );
            update(model, UiEvent::SendDone(Ok(())));
        }
        Err(error) => {
            update(model, UiEvent::SendDone(Err(error)));
        }
    }
}
