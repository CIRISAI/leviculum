//! The node a Loquax handheld runs, and its client of a home (1414,
//! slice four of `docs/src/concepts/handheld-messenger.md`, "Networking: a
//! client of a home").
//!
//! One [`NodeCore`] with one interface, the host's serial line
//! ([`crate::serial_iface`]): on the bench that is schneckenschreck's
//! lnsd, from there the rendezvous and the home. The LR1110 in Mesh mode is
//! the second interface and the next step; nothing here knows which
//! interface a packet took.
//!
//! The client is `loquax::home::board`: a pending record joins, an
//! admitted one sends what the board's model typed and pulls from its
//! cursor whenever the session says an ask is due, and every page and
//! every end is said on the debug line as a `[HOME]` line. A code typed on
//! the keys or written over the control link (`TYPE_HOME_CODE`, 1415)
//! makes the record ([`Handheld::take_code`]). A page's entries are handed
//! to the board's loop ([`Handheld::drain`]), which folds them into the
//! model the screen draws (`loquax_m9::home`), and each page is kept in
//! the message store in flash as it comes (1476). At boot the model is
//! restored from the store and the record's cursor is made to agree with
//! it ([`Handheld::client`]): a board whose store is gone pulls again
//! instead of trusting a cursor that says everything was fetched.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use esp_hal::rng::Trng;
use leviculum_core::ifac::IfacConfig;
use leviculum_core::node::{NodeCore, NodeCoreBuilder};
use leviculum_core::traits::Interface;
use leviculum_core::transport::dispatch_actions;
use leviculum_core::{EmbeddedStorage, Identity, InterfaceMode, TickOutput};
use loquax::home::ask::{Ask, HomeError, Outcome, Verb};
use loquax::home::board::{
    self, Batch, Executor, LinkClass, Progress, Reach, Report, Session, Word,
};
use loquax::home::record::{Admission, ClientRecord};
use loquax::home::wire::{Entry, Submission};
use rand_core::CryptoRngCore;

use loquax::home::board::store::Store;
use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::home as m9home;

use crate::flash::Stores;
use crate::serial_iface::{SerialIface, SERIAL_HW_MTU, SERIAL_IFACE, SERIAL_NAME};
use crate::{EspClock, UartLog};

/// The node's concrete type.
pub type Core = NodeCore<Trng, EspClock, EmbeddedStorage>;

/// The most links the handheld keeps: its home's, a peer's, room for one
/// more. The heap is PSRAM; the bound is the link table's, not memory's.
pub const MAX_LINKS: usize = 8;

/// The largest message the board takes in as one answer or resource: the
/// pull batch's answer ceiling (`Batch::max_answer`). LXMF's own default
/// for a message over a link is far under it; PSRAM has the room.
pub const MAX_ANSWER: usize = 256 * 1024;

/// How long a join may take: the path ladder's first five rungs.
pub const JOIN_BUDGET_MS: u64 = 120_000;
/// How long one pull may take, every page included.
pub const PULL_BUDGET_MS: u64 = 300_000;
/// How long one send may take: the ladder's first rungs and a link.
pub const SEND_BUDGET_MS: u64 = 120_000;

/// The name the board joins its home under: what the phone's device list
/// shows (`loquax_m9::pair::DEVICE_NAME`).
pub const DEVICE_NAME: &str = "M9";

/// Why a code was not taken, as the control envelope refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeRefusal {
    /// Not a home code.
    Value,
    /// The record did not reach flash.
    Persist,
}

/// What the client has for the board's loop since it last asked.
#[derive(Debug, Default)]
pub struct Drained {
    /// Pulled entries, oldest first, for the model.
    pub entries: Vec<Entry>,
    /// Sends that ended, oldest first: the message id the home gave, or
    /// why it refused.
    pub sent: Vec<Result<[u8; 32], String>>,
}

/// The node, its serial interface and its client.
pub struct Handheld {
    pub node: Box<Core>,
    pub serial: SerialIface,
    ifac: BTreeMap<usize, IfacConfig>,
    client: Option<Client>,
}

/// A device of a home: its record, the identity it was admitted as, the
/// ask in flight and when to ask next.
struct Client {
    record: ClientRecord,
    device: Identity,
    /// The home's LXMF address, which the board's model shows as its own.
    address: [u8; 16],
    batch: Batch,
    session: Session,
    exec: Option<Executor>,
    /// The ask in flight is the oldest send.
    sending: bool,
    /// Entries taken in the pull in flight.
    pulled: usize,
    /// How far the latest pull came, for the screen's word.
    progress: Option<Progress>,
    /// The cursor as flash has it; the record's moves with every page.
    saved_cursor: u64,
    /// The message store, when the board's table has one (1476).
    store: Option<Store>,
    /// Sends the model asked for, oldest first.
    outgoing: VecDeque<Submission>,
    drained: Drained,
}

/// The node's identity: the one in flash, or a new one saved there.
/// Says which in `[IDENTITY]`.
pub fn identity(stores: &mut Stores<'_>, rng: &mut Trng, log: &mut UartLog<'_>) -> Identity {
    let (identity, source) = match stores.load_identity() {
        Ok(Some(identity)) => (identity, "flash"),
        loaded => {
            if let Err(error) = loaded {
                log.line("[FLASH] ", format_args!("identity=unread error={error}"));
            }
            (Identity::generate(rng), "generated")
        }
    };
    let stored = match source {
        "flash" => "yes",
        _ => match stores.save_identity(&identity) {
            Ok(()) => "yes",
            Err(error) => {
                log.line("[FLASH] ", format_args!("identity=unsaved error={error}"));
                "no"
            }
        },
    };
    let mut hash = [0u8; 32];
    for (i, byte) in identity.hash().iter().enumerate() {
        hash[2 * i] = hex(byte >> 4);
        hash[2 * i + 1] = hex(byte & 0xF);
    }
    log.line(
        "[IDENTITY] ",
        format_args!(
            "identity={} probe=none lxmf=none source={source} stored={stored}",
            core::str::from_utf8(&hash).unwrap_or("?")
        ),
    );
    identity
}

fn hex(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + nibble - 10,
    }
}

impl Handheld {
    /// The node over `identity`, its serial interface up.
    pub fn new(identity: Identity, rng: Trng) -> Self {
        let mut node = NodeCoreBuilder::new()
            .identity(identity)
            .enable_transport(true)
            .max_links(Some(MAX_LINKS))
            .max_incoming_resource_size(MAX_ANSWER)
            .build_boxed(rng, EspClock, EmbeddedStorage::new());
        node.set_interface_name(SERIAL_IFACE.0, alloc::string::String::from(SERIAL_NAME));
        node.set_interface_hw_mtu(SERIAL_IFACE.0, SERIAL_HW_MTU as u32);
        // The host behind the line asks paths for itself; a Gateway
        // interface discovers them on its behalf (#117), as an LNode's.
        node.set_interface_mode(SERIAL_IFACE.0, InterfaceMode::Gateway);
        let mut handheld = Self {
            node,
            serial: SerialIface::new(),
            ifac: BTreeMap::new(),
            client: None,
        };
        let up = handheld.node.handle_interface_up(SERIAL_IFACE.0);
        handheld.dispatch(up.actions);
        handheld
    }

    /// Take `text` as a home's admission code (1415): a new device
    /// identity from `rng`, the pending record written to flash, and the
    /// client that joins with it, replacing any it had; the model the
    /// screen shows from here. Typed on the keys or written by `lnflash
    /// --home`; the same record either way.
    pub fn take_code(
        &mut self,
        text: &str,
        rng: &mut impl CryptoRngCore,
        stores: &mut Stores<'_>,
        batch: Batch,
        log: &mut UartLog<'_>,
    ) -> Result<Option<UiModel<Field>>, CodeRefusal> {
        let device = Identity::generate(rng);
        let record = match board::admit(text, &device, DEVICE_NAME) {
            Ok(record) => record,
            Err(error) => {
                log.line("[HOME] ", format_args!("code=refused error={error:?}"));
                return Err(CodeRefusal::Value);
            }
        };
        if let Err(error) = stores.save_client(&record) {
            log.line("[HOME] ", format_args!("code=unsaved error={error}"));
            return Err(CodeRefusal::Persist);
        }
        log.line("[HOME] ", format_args!("code=taken saved=yes"));
        Ok(self.client(record, batch, stores, log))
    }

    /// Become a device of the home `record` names, pulling in `batch`,
    /// and the model the screen shows: what the message store in flash
    /// holds for that home (1476), said in a `[STORE]` line. `None`, and
    /// why in a `[HOME]` line, when the record's identity does not read.
    ///
    /// The record's cursor is then made to agree with the store
    /// (`board::agree`): a cursor past zero over an empty store means the
    /// store was lost, and the board pulls again from the start rather
    /// than showing an empty list under a cursor that says everything was
    /// fetched. The record in flash keeps its old cursor until the next
    /// pull ends and writes the new one.
    pub fn client(
        &mut self,
        mut record: ClientRecord,
        batch: Batch,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) -> Option<UiModel<Field>> {
        let address = match loquax::home::lxmf_address(&record.home) {
            Ok(address) => address,
            Err(error) => {
                log.line("[HOME] ", format_args!("client=unread error={error}"));
                return None;
            }
        };
        let t0 = crate::uptime_ms();
        let (model, store) = match stores.messages() {
            None => {
                log.line(
                    "[STORE] ",
                    format_args!("sectors=0 region={}", stores.messages_slot()),
                );
                (m9home::model(address), None)
            }
            Some(mut dev) => match m9home::restore(&mut dev, address) {
                Ok((model, store, restored)) => {
                    log.line(
                        "[STORE] ",
                        format_args!(
                            "sectors={} used={} entries={} cursor={} torn={} foreign={} restore_ms={}",
                            store.sectors(),
                            store.used(),
                            restored.entries,
                            store.cursor(),
                            restored.torn,
                            restored.foreign,
                            crate::uptime_ms().saturating_sub(t0)
                        ),
                    );
                    (model, Some(store))
                }
                Err(error) => {
                    log.line("[STORE] ", format_args!("restore=failed error={error}"));
                    (m9home::model(address), None)
                }
            },
        };
        let stored = store.as_ref().map_or(0, Store::cursor);
        let saved_cursor = record.cursor;
        let agreement = board::agree(&mut record, stored);
        if agreement != board::Agreement::Agrees {
            log.line(
                "[HOME] ",
                format_args!("{agreement} cursor={}", record.cursor),
            );
        }
        match record.device_identity() {
            Ok(device) => {
                log.line(
                    "[HOME] ",
                    format_args!(
                        "client=yes admitted={} cursor={} page={} max_answer={}",
                        if record.admitted() { "yes" } else { "no" },
                        record.cursor,
                        batch.page_bytes,
                        batch.max_answer
                    ),
                );
                self.client = Some(Client {
                    saved_cursor,
                    record,
                    device,
                    address,
                    batch,
                    session: Session::new(),
                    exec: None,
                    sending: false,
                    pulled: 0,
                    progress: None,
                    outgoing: VecDeque::new(),
                    drained: Drained::default(),
                    store,
                });
                Some(model)
            }
            Err(error) => {
                log.line("[HOME] ", format_args!("client=unread error={error:?}"));
                None
            }
        }
    }

    /// The home's LXMF address, the board's own in its model; `None` on a
    /// board that is no device of a home.
    pub fn address(&self) -> Option<[u8; 16]> {
        self.client.as_ref().map(|client| client.address)
    }

    /// What the screen's top bar says of the home.
    pub fn word(&self) -> Option<Word> {
        self.client.as_ref().map(|client| {
            let admitted = client.record.admitted();
            Word::of(&client.session, admitted, client.progress)
        })
    }

    /// Sends the model asked for: queued, and asked of the home at once.
    pub fn send(&mut self, submissions: Vec<Submission>, now_ms: u64) {
        if submissions.is_empty() {
            return;
        }
        if let Some(client) = self.client.as_mut() {
            client.outgoing.extend(submissions);
            client.session.hurry(now_ms);
        }
    }

    /// What the client has for the loop since the last call.
    pub fn drain(&mut self) -> Drained {
        self.client
            .as_mut()
            .map(|client| core::mem::take(&mut client.drained))
            .unwrap_or_default()
    }

    /// Whether the home answers, for the screen; `None` on a board that
    /// is no device of a home.
    pub fn reach(&self) -> Option<Reach> {
        self.client.as_ref().map(|client| client.session.reach())
    }

    /// A packet the host sent.
    pub fn packet(
        &mut self,
        data: &[u8],
        now_ms: u64,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) {
        let out = self.node.handle_packet(SERIAL_IFACE, data);
        self.output(out, now_ms, stores, log);
    }

    /// One turn of the loop: the node's timers, the client's next step,
    /// and every frame the node queued written to the line.
    pub fn turn(&mut self, now_ms: u64, stores: &mut Stores<'_>, log: &mut UartLog<'_>) {
        if self.node.next_deadline().is_some_and(|due| due <= now_ms) {
            let out = self.node.handle_timeout();
            self.output(out, now_ms, stores, log);
        }
        let mut out = TickOutput::default();
        let report = match self.client.as_mut() {
            None => Report::Idle,
            Some(client) => {
                if client.exec.is_none() && client.session.due(now_ms) {
                    client.start(now_ms, log);
                }
                match client.exec.as_mut() {
                    Some(exec) => exec.poll(&mut self.node, &client.device, now_ms, &mut out),
                    None => Report::Idle,
                }
            }
        };
        self.report(report, now_ms, stores, log);
        self.output(out, now_ms, stores, log);
        while let Some(frame) = self.serial.pop() {
            log.frame(&frame);
        }
    }

    fn dispatch(&mut self, actions: Vec<leviculum_core::Action>) {
        if actions.is_empty() {
            return;
        }
        let mut ifaces: [&mut dyn Interface; 1] = [&mut self.serial];
        let dispatched = dispatch_actions(&mut ifaces, actions, &self.ifac);
        self.node.record_dispatch_drops(&dispatched);
    }

    /// The node's output: its packets to the interface, its events to the
    /// client, whose own output goes the same way.
    fn output(
        &mut self,
        out: TickOutput,
        now_ms: u64,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) {
        let mut pending = alloc::vec![out];
        while let Some(out) = pending.pop() {
            self.dispatch(out.actions);
            for event in &out.events {
                let mut more = TickOutput::default();
                let report = match self.client.as_mut() {
                    Some(Client {
                        exec: Some(exec),
                        device,
                        ..
                    }) => exec.event(&mut self.node, device, event, now_ms, &mut more),
                    _ => Report::Idle,
                };
                self.report(report, now_ms, stores, log);
                pending.push(more);
            }
        }
    }

    fn report(
        &mut self,
        report: Report,
        now_ms: u64,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match report {
            Report::Idle => {}
            Report::Page(page) => {
                client.pulled += page.entries.len();
                client
                    .progress
                    .get_or_insert_with(Progress::default)
                    .page(&page);
                let last = page.entries.last().map_or(0, |entry| entry.seq);
                // Taken, so a pull that breaks after this page asks again
                // from here, in this ask or the next one.
                client.record.cursor = client.record.cursor.max(last);
                let bytes: usize = page.entries.iter().map(|entry| entry.packed.len()).sum();
                let progress = client.progress.unwrap_or_default();
                // The link the page came over and the page the board asked
                // on it (1439): one packet on a radio, a second of the line
                // on serial.
                let (link, asked) = match client.exec.as_ref().and_then(Executor::fit) {
                    Some((LinkClass::Radio, page)) => ("radio", page),
                    Some((LinkClass::Serial, page)) => ("serial", page),
                    Some((LinkClass::Wide, page)) => ("wide", page),
                    None => ("unfitted", client.batch.page_bytes),
                };
                log.line(
                    "[HOME] ",
                    format_args!(
                        "page entries={} last={last} bytes={bytes} more={} taken={} of={} link={link} asked={asked}",
                        page.entries.len(),
                        page.more,
                        progress.taken,
                        progress.of.map_or(-1, |of| of as i64)
                    ),
                );
                // Kept in flash before the model takes it (1476): one write
                // per sector the page touches, before the next page is
                // asked, so the line is quiet while the flash is busy.
                if let (Some(store), Some(mut dev)) = (client.store.as_mut(), stores.messages()) {
                    match m9home::keep(store, &mut dev, &page.entries, &client.address) {
                        Ok(done) => log.line(
                            "[STORE] ",
                            format_args!(
                                "kept={} writes={} erased={} cursor={} used={} entries={}",
                                done.entries,
                                done.writes,
                                done.erased,
                                store.cursor(),
                                store.used(),
                                store.entries()
                            ),
                        ),
                        Err(error) => log.line("[STORE] ", format_args!("kept=no error={error}")),
                    }
                }
                client.drained.entries.extend(page.entries);
            }
            Report::Done(result) => {
                client.exec = None;
                client.session.finished(now_ms, &result);
                client.done(&result, now_ms, stores, log);
            }
        }
    }
}

impl Client {
    /// Begin the ask the record owes: its join while pending; once
    /// admitted the oldest send, else a pull.
    fn start(&mut self, now_ms: u64, log: &mut UartLog<'_>) {
        self.sending = false;
        let ask = match (&self.record.admission, self.outgoing.front()) {
            (Admission::Pending { .. }, _) => self.record.join(now_ms, JOIN_BUDGET_MS),
            (Admission::Admitted, Some(submission)) => {
                self.sending = true;
                let verb = Verb::Send(submission.clone());
                Some(Ask::new(&self.record.home, verb, now_ms, SEND_BUDGET_MS))
            }
            (Admission::Admitted, None) => {
                self.record
                    .pull(self.batch.page_bytes, now_ms, PULL_BUDGET_MS)
            }
        };
        match ask {
            Some(Ok(ask)) => {
                self.pulled = 0;
                if !self.sending {
                    self.progress = matches!(self.record.admission, Admission::Admitted)
                        .then(Progress::default);
                }
                // A pull's page follows the link it gets (1439); a join's
                // and a send's answers are one packet whatever the link.
                self.exec = Some(Executor::fitted(ask, self.batch));
            }
            Some(Err(error)) => {
                log.line("[HOME] ", format_args!("ask=refused error={error}"));
                self.session.finished(now_ms, &Err(error));
            }
            None => {}
        }
    }

    fn done(
        &mut self,
        result: &Result<Outcome, HomeError>,
        now_ms: u64,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) {
        let reach = match self.session.reach() {
            Reach::Unknown => "unknown",
            Reach::Connected => "connected",
            Reach::Unreachable => "unreachable",
            Reach::Refused(_) => "refused",
        };
        let next_in = self.session.next_ms().saturating_sub(now_ms);
        match result {
            Ok(Outcome::Joined) => {
                self.record.admission = Admission::Admitted;
                self.session.hurry(now_ms);
                let saved = stores.save_client(&self.record);
                log.line(
                    "[HOME] ",
                    format_args!(
                        "joined=yes reach={reach} saved={}",
                        if saved.is_ok() { "yes" } else { "no" }
                    ),
                );
            }
            Ok(Outcome::Pulled {
                cursor,
                entries,
                pages,
                ..
            }) => {
                self.record.cursor = self.record.cursor.max(*cursor);
                let saved = self.save_cursor(stores);
                log.line(
                    "[HOME] ",
                    format_args!(
                        "pulled entries={entries} pages={pages} cursor={cursor} saved={saved} reach={reach} next_in_ms={next_in}"
                    ),
                );
            }
            Ok(Outcome::Sent(id)) => {
                self.outgoing.pop_front();
                self.drained.sent.push(Ok(*id));
                // The home archived it: the next pull brings its entry,
                // which names the bubble on every device.
                self.session.hurry(now_ms);
                log.line(
                    "[HOME] ",
                    format_args!(
                        "sent id={:02x}{:02x}{:02x}{:02x} waiting={} reach={reach}",
                        id[0],
                        id[1],
                        id[2],
                        id[3],
                        self.outgoing.len()
                    ),
                );
            }
            Ok(other) => log.line("[HOME] ", format_args!("done={other:?} reach={reach}")),
            Err(error) if self.sending => {
                // A refusal stays one; anything else is asked again when
                // the session says, the send kept at the front.
                if matches!(error, HomeError::Refused(_)) {
                    self.outgoing.pop_front();
                    self.drained.sent.push(Err(alloc::format!("{error}")));
                }
                log.line(
                    "[HOME] ",
                    format_args!(
                        "send=failed error=\"{error}\" waiting={} reach={reach} next_in_ms={next_in}",
                        self.outgoing.len()
                    ),
                );
            }
            Err(error) => {
                let saved = self.save_cursor(stores);
                log.line(
                    "[HOME] ",
                    format_args!(
                        "failed error=\"{error}\" pulled={} cursor={} saved={saved} reach={reach} failures={} next_in_ms={next_in}",
                        self.pulled,
                        self.record.cursor,
                        self.session.failures()
                    ),
                );
            }
        }
    }

    /// Write the record when its cursor moved since the last write: once
    /// per pull, not once per page, for the flash's sake.
    fn save_cursor(&mut self, stores: &mut Stores<'_>) -> &'static str {
        if self.record.cursor == self.saved_cursor {
            return "unchanged";
        }
        match stores.save_client(&self.record) {
            Ok(()) => {
                self.saved_cursor = self.record.cursor;
                "yes"
            }
            Err(_) => "no",
        }
    }
}
