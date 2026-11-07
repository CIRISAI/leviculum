//! The M9 as a device of its home, on the model's side (1415): the model a
//! board that joined a home starts from, the archive's pages folded into
//! it (`loquax::home::board::fold`), and the sends the model asks for
//! turned into the home's `/send`. The board's loop
//! (`leviculum_esp::handheld`) performs the asks; this is what it hands
//! the model and takes from it, `no_std` and tested on the host.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use loquax::home::ask::{GroupVerb, HomeError, Outcome, Verb};
use loquax::home::board;
use loquax::home::board::store::{self, Appended, PendingAsk, Restored, Sectors, Store};
use loquax::home::wire::{Entry, Mark, Submission};
use loquax::ui::{groups, update, Batch, Pane, UiEffect, UiEvent, UiModel};

use crate::field::Field;

/// The model of a board that is a device of the home whose LXMF address
/// is `address`: the home's identity is the board's, so what the home took
/// in is the board's own post. Empty until the first pull.
pub fn model(address: [u8; 16]) -> UiModel<Field> {
    let mut model = UiModel::<Field>::new(&[]);
    model.lang = crate::LANG;
    model.address = Some(address);
    model.focus = Pane::Conversations;
    // The board is a device of its home (1493): without this the model
    // queued no read marks (`read_out` returns early), and what was read
    // on the board stayed unread everywhere else.
    model.home_client = true;
    // The list is a screen of its own, as on a phone (1493): the M9's
    // Compact scene shows the list OR the chat, never both, so an
    // arrival while the list is up must stay unread ([`take`] hides the
    // chat from the fold while the keys are on the list).
    model.list_apart = true;
    // The board's screen (1490): the scene builds its window, not the
    // store.
    model.viewing = crate::screen::viewing();
    // The board's map, as the fixture has it (1476): without it the map
    // a home board opens is sized for a terminal, not the picture.
    crate::map::ready(&mut model);
    model
}

/// Fold a page's entries into `model` as one batch (1514,
/// [`Batch`]): the list is ordered and the screen's rows built once
/// for the page, not once per entry. How many it took, and what the
/// model asks back (1493): a receipt due because the chat is open, a
/// mark — the loop routes them ([`route`]), it drops nothing.
pub fn take(
    model: &mut UiModel<Field>,
    entries: &[Entry],
    address: &[u8; 16],
) -> (usize, Vec<UiEffect>) {
    // The Compact scene shows the list or the chat, never both (1493):
    // while the keys are on the list the selected chat is not on the
    // panel, and an arrival there must not be marked "read where it
    // arrived" — the fold sees no open conversation, as a phone's closed
    // chat does. The desktop's rule stands where the chat is shown.
    let hidden = model.focus == Pane::Conversations;
    let was = model.selected;
    if hidden {
        model.selected = loquax::ui::NONE_OPEN;
    }
    let mut taken = 0;
    let mut batch = Batch::new(model);
    for event in entries
        .iter()
        .filter_map(|entry| board::fold(entry, address))
    {
        batch.fold(UiEvent::Engine(event));
        taken += 1;
    }
    let effects = batch.finish();
    if hidden {
        model.selected = was;
    }
    (taken, effects)
}

/// What a board that is a device of the home at `address` shows at boot
/// (1476): its model with every entry its message store on `dev` holds
/// folded in, the store, and what the store found. The client's cursor
/// then agrees with [`Store::cursor`] (`board::agree`). One batch
/// (1514): every entry folded, the list ordered and the screen's rows
/// built once at the end; per entry that cost O(conversations) and a
/// 4067-entry store 17 minutes on the board.
/// What a boot's restore hands back (1493): the model, the store, what
/// the store found, and the asks still owed the home.
pub type Booted = (UiModel<Field>, Store, Restored, Vec<PendingAsk>);

pub fn restore<S: Sectors>(
    dev: &mut S,
    address: [u8; 16],
) -> Result<Booted, S::Error> {
    let mut shown = model(address);
    // No chat is on the panel at boot: the fold must not read anything
    // "where it arrived" ([`take`]'s rule; the restored read state is the
    // kept marks', nothing else's).
    shown.selected = loquax::ui::NONE_OPEN;
    let mut batch = Batch::new(&mut shown);
    let opened = Store::open(dev, store::tag(&address), |entry| {
        if let Some(event) = board::fold(&entry, &address) {
            batch.fold(UiEvent::Engine(event));
        }
    });
    batch.finish();
    shown.selected = 0;
    let (store, restored, asks) = opened?;
    // The fold just said marks for what it restored as read; they are the
    // store's own entries played back, nothing the home still owes.
    let _ = shown.take_marks();
    let _ = shown.take_writes();
    Ok((shown, store, restored, asks))
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

/// What a fold's output becomes on the board (1493): the verbs for the
/// ask queue, the marks among them (kept locally too, [`keep_marks`]),
/// the lines for the log, and what the loop does itself. Nothing is
/// dropped silently: an effect the board has no platform for is named.
#[derive(Debug, Default)]
pub struct Routed {
    /// For the ask queue, oldest first: sends, marks, group verbs.
    pub verbs: Vec<Verb>,
    /// The marks among the verbs, as the model said them: written into
    /// the message store as well, so read state survives a reboot that
    /// comes before the home heard it.
    pub marks: Vec<Mark>,
    /// Lines for the debug log: the model's notices and log asks.
    pub notices: Vec<String>,
    /// Effects the board cannot perform, each by name, for the log.
    pub unrouted: Vec<&'static str>,
    /// The screen owes a full redraw (`Ctrl-l`).
    pub redraw: bool,
}

/// Route everything a fold left behind (1493): `effects` as [`update`]
/// returned them, and the queues the desktop's shell drains each fold
/// (`take_marks`, `take_writes`, petnames, peer states), which grew until
/// reboot on a board that never drained them. Marks become `/mark` asks;
/// a send and a group verb their asks, each under a fresh id from `ids`
/// (the same id asked again is the same message at the home). Store
/// writes are drained and dropped: the message store in flash already
/// holds what they would write, entry by entry. Petnames and peer states
/// have no ask on the board yet and are named in `unrouted`.
pub fn route(
    model: &mut UiModel<Field>,
    effects: Vec<UiEffect>,
    ids: &mut impl FnMut() -> [u8; 16],
) -> Routed {
    let mut out = Routed::default();
    for mark in model.take_marks() {
        out.verbs.push(Verb::Mark(mark.clone()));
        out.marks.push(mark);
    }
    let _ = model.take_writes();
    if !model.take_petnames().is_empty() {
        out.unrouted.push("petnames");
    }
    if !model.take_peer_states().is_empty() {
        out.unrouted.push("peer-states");
    }
    for effect in effects {
        match effect {
            UiEffect::CreateGroup { .. }
            | UiEffect::AddMembers { .. }
            | UiEffect::DeleteGroup { .. } => {
                if let Some(verb) = crate::group::ask_of(&effect, ids()) {
                    out.verbs.push(Verb::Group(verb));
                }
            }
            UiEffect::Send { .. } => {
                let id = ids();
                match crate::group::ask_of(&effect, id) {
                    Some(verb) => out.verbs.push(Verb::Group(verb)),
                    None => {
                        let UiEffect::Send { to, body, fields } = effect else {
                            unreachable!()
                        };
                        out.verbs.push(Verb::Send(Submission {
                            client_id: id,
                            destination: to,
                            title: Vec::new(),
                            content: body.into_bytes(),
                            fields,
                        }));
                    }
                }
            }
            UiEffect::Notice(line) | UiEffect::Log(line) => out.notices.push(line),
            UiEffect::PnLine(line) => out.notices.push(line),
            UiEffect::Redraw => out.redraw = true,
            other => out.unrouted.push(effect_name(&other)),
        }
    }
    out
}

/// The word an effect the board cannot perform is logged under.
fn effect_name(effect: &UiEffect) -> &'static str {
    match effect {
        UiEffect::Network { .. } => "network",
        UiEffect::Send { .. } => "send",
        UiEffect::Fetch => "fetch",
        UiEffect::AskPn { .. } => "ask-pn",
        UiEffect::PnLine(_) => "pn-line",
        UiEffect::Quit => "quit",
        UiEffect::Redraw => "redraw",
        UiEffect::Copy(_) => "copy",
        UiEffect::Browse(_) => "browse",
        UiEffect::Rooms(_) => "rooms",
        UiEffect::Contact(_) => "contact",
        UiEffect::Open(_) => "open",
        UiEffect::Mouse(_) => "mouse",
        UiEffect::Notice(_) => "notice",
        UiEffect::Store(_) => "store-read",
        UiEffect::Log(_) => "log",
        UiEffect::LoadMap(_) => "load-map",
        UiEffect::MapDrawing(_) => "map-drawing",
        UiEffect::Import => "import",
        UiEffect::Remember { .. } => "remember",
        UiEffect::ShowPhoto { .. } => "show-photo",
        UiEffect::OpenFile { .. } => "open-file",
        UiEffect::PlayVoice { .. } => "play-voice",
        UiEffect::PauseVoice { .. } => "pause-voice",
        UiEffect::CreateGroup { .. } => "create-group",
        UiEffect::AddMembers { .. } => "add-members",
        UiEffect::DeleteGroup { .. } => "delete-group",
        UiEffect::DeleteAllMaps => "delete-all-maps",
        UiEffect::Attach { .. } => "attach",
        UiEffect::ScanCode { .. } => "scan-code",
        UiEffect::Recording { .. } => "recording",
    }
}

/// Keep the board's own marks in the message store (1493): each as a
/// [`loquax::home::KIND_MARK`] entry under sequence 0, which the restore
/// folds back the way a pulled mark folds, so what was read here is read
/// after a reboot — home heard from or not. The store's cursor does not
/// move. Folding the home's archived copy of the same mark later is
/// idempotent.
pub fn keep_marks<S: Sectors>(
    store: &mut Store,
    dev: &mut S,
    marks: &[Mark],
) -> Result<Appended, S::Error> {
    if marks.is_empty() {
        return Ok(Appended::default());
    }
    let entries: Vec<Entry> = marks
        .iter()
        .map(|mark| Entry {
            seq: 0,
            kind: loquax::home::KIND_MARK,
            at: 0,
            packed: loquax::home::wire::mark_request(mark),
        })
        .collect();
    let cursor = store.cursor();
    store.append(dev, &entries, cursor)
}

/// The verbs waiting for the home, oldest first, asked one at a time,
/// each under the id its store frame carries (1493).
#[derive(Debug, Default)]
pub struct Outgoing {
    queue: VecDeque<([u8; 16], Verb)>,
}

impl Outgoing {
    pub fn push(&mut self, id: [u8; 16], verb: Verb) {
        self.queue.push_back((id, verb));
    }

    /// The oldest, still queued until [`Outgoing::done`].
    pub fn next(&self) -> Option<&([u8; 16], Verb)> {
        self.queue.front()
    }

    /// The oldest was asked and is over: taken or given up.
    pub fn done(&mut self) -> Option<([u8; 16], Verb)> {
        self.queue.pop_front()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// How an ask ended, carried from the client's executor to the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// A send: the message id the home gave it, or why not.
    Sent(Result<[u8; 32], String>),
    /// A mark: nothing to show; an error is a line.
    Marked(Result<(), String>),
    /// `/group`: the group's address, or why it was not made.
    GroupMade(Result<[u8; 16], String>),
    /// `/groupadd`.
    MembersAdded(Result<[u8; 16], String>),
    /// `/groupdelete`.
    GroupDeleted(Result<[u8; 16], String>),
}

/// What `result` answers the `verb` that was asked; `None` for a verb
/// the queue never carries (a join, a pull).
pub fn answer_of(verb: &Verb, result: &Result<Outcome, HomeError>) -> Option<Answer> {
    let error = |e: &HomeError| alloc::format!("{e}");
    Some(match verb {
        Verb::Send(_) | Verb::Group(GroupVerb::State(_)) => Answer::Sent(match result {
            Ok(Outcome::Sent(id)) => Ok(*id),
            Ok(other) => Err(alloc::format!("{other:?}")),
            Err(e) => Err(error(e)),
        }),
        Verb::Mark(_) => Answer::Marked(match result {
            Ok(_) => Ok(()),
            Err(e) => Err(error(e)),
        }),
        Verb::Group(GroupVerb::Create(_)) => Answer::GroupMade(group_result(result)),
        Verb::Group(GroupVerb::Add(_)) => Answer::MembersAdded(group_result(result)),
        Verb::Group(GroupVerb::Delete(delete)) => Answer::GroupDeleted(match result {
            Ok(_) => Ok(delete.group),
            Err(e) => Err(error(e)),
        }),
        Verb::Join { .. } | Verb::Pull { .. } => return None,
    })
}

fn group_result(result: &Result<Outcome, HomeError>) -> Result<[u8; 16], String> {
    match result {
        Ok(Outcome::Group(group)) => Ok(*group),
        Ok(other) => Err(alloc::format!("{other:?}")),
        Err(e) => Err(alloc::format!("{e}")),
    }
}

/// Fold `answer` into `model`: the bubble named, the group screen told,
/// as the desktop's runner tells them. What the model asks back goes
/// through [`route`] like any fold's effects.
pub fn answered(model: &mut UiModel<Field>, answer: Answer) -> Vec<UiEffect> {
    match answer {
        Answer::Sent(Ok(message_id)) => {
            let mut effects = update(
                model,
                UiEvent::Engine(loquax::outbox::OutboxEvent::Queued { message_id }),
            );
            effects.extend(update(model, UiEvent::SendDone(Ok(()))));
            effects
        }
        Answer::Sent(Err(e)) => update(model, UiEvent::SendDone(Err(e))),
        Answer::Marked(_) => Vec::new(),
        Answer::GroupMade(made) => update(
            model,
            UiEvent::GroupMade(made.map_err(groups::NotMade::Failed)),
        ),
        Answer::MembersAdded(added) => update(model, UiEvent::MembersAdded(added)),
        Answer::GroupDeleted(deleted) => update(model, UiEvent::GroupDeleted(deleted)),
    }
}
