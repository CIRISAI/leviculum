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
use alloc::collections::BTreeMap;
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
use loquax::home::wire::{Entry, Mark};
use rand_core::CryptoRngCore;

use loquax::ui::listing::Row;
use loquax::ui::UiModel;
use loquax_m9::field::Field;
use loquax_m9::home as m9home;

/// What a restore does with the bench's arrivals (1562): the bench image
/// lists "Bench", a release image never does.
#[cfg(m9_bench)]
const ARRIVALS: loquax_m9::bench::Arrivals = loquax_m9::bench::Arrivals::Shown;
#[cfg(not(m9_bench))]
const ARRIVALS: loquax_m9::bench::Arrivals = loquax_m9::bench::Arrivals::Dropped;
use loquax_m9::home::{Answer, How, Outgoing, Shelf};

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
    /// Asks that ended, oldest first: what the home answered, for the
    /// model (`m9home::answered`).
    pub answers: Vec<Answer>,
    /// Writes that did not reach the flash since the last drain (1493):
    /// a kept page, an ask frame, a mark. The screen says it as a fault
    /// word, never only the log.
    pub unsaved: u32,
    /// The join went through but its record did not reach the flash
    /// (1493): a reboot lands on the pairing screen with a used token,
    /// and the screen must say so before that happens.
    pub join_unsaved: bool,
}

/// The node, its serial interface and its client.
pub struct Handheld {
    pub node: Box<Core>,
    pub serial: SerialIface,
    ifac: BTreeMap<usize, IfacConfig>,
    client: Option<Client>,
    /// The bench's owe, armed and not fired yet (1540): its flags.
    #[cfg(m9_bench)]
    owe: Option<u8>,
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
    /// The message store, when the board's table has one (1476), with
    /// its summary log (1521).
    store: Option<Shelf>,
    /// The verbs the model asked for, oldest first, each under the id
    /// its store frame carries (1493).
    outgoing: Outgoing,
    drained: Drained,
}

/// The node's identity: the one in flash, or, where none was ever saved,
/// a new one saved there. Says which in `[IDENTITY]`.
///
/// A flash READ error keeps the board without an identity (1493): the
/// identity the mesh and the home know may still be in that sector, and
/// a generated one written over it would replace it for good over a
/// transient fault. The board says so on its screen and runs no node.
pub fn identity(
    stores: &mut Stores<'_>,
    rng: &mut Trng,
    log: &mut UartLog<'_>,
) -> Option<Identity> {
    let (identity, source) = match stores.load_identity() {
        Ok(Some(identity)) => (identity, "flash"),
        Ok(None) => (Identity::generate(rng), "generated"),
        Err(error) => {
            log.line("[FLASH] ", format_args!("identity=unread error={error}"));
            return None;
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
    Some(identity)
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
            #[cfg(m9_bench)]
            owe: None,
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
        feed: &dyn Fn(),
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
        Ok(self.client(record, batch, stores, log, feed))
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
    /// fetched. The record in flash is written when the board joins and
    /// when a code makes a new one, never for its cursor (1463): the store
    /// holds the cursor, and this agreement is the only reader of the
    /// record's.
    ///
    /// `feed` keeps the caller's watchdog fed through the restore (1521c):
    /// a restore that folds the whole ring took 57 s to 100 s on the
    /// bench, one entry at a time, and core 0's watchdog resets the chip
    /// after 5 s unfed. The restore's clock calls it, several times per
    /// entry; at boot, before the watchdog is armed, it pumps the line's
    /// bytes to the heap (1603, `pump_rx`), as every caller's does.
    pub fn client(
        &mut self,
        mut record: ClientRecord,
        batch: Batch,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
        feed: &dyn Fn(),
    ) -> Option<UiModel<Field>> {
        let address = match loquax::home::lxmf_address(&record.home) {
            Ok(address) => address,
            Err(error) => {
                log.line("[HOME] ", format_args!("client=unread error={error}"));
                return None;
            }
        };
        let t0 = crate::uptime_ms();
        let mut outgoing = Outgoing::default();
        let (model, store) = match stores.messages() {
            None => {
                log.line(
                    "[STORE] ",
                    format_args!("sectors=0 region={}", stores.messages_slot()),
                );
                (m9home::model(address), None)
            }
            Some(mut dev) => match m9home::restore_chunked(
                &mut dev,
                address,
                &|| {
                    feed();
                    crate::uptime_us()
                },
                ARRIVALS,
                // A rebuild's progress (1603): window 22's reopen folded
                // for twelve silent seconds before its panic.
                &mut |entries| {
                    log.line(
                        "[STORE] ",
                        format_args!(
                            "rebuild chunk={} entries={entries} ms={}",
                            entries / m9home::REBUILD_CHUNK,
                            crate::uptime_ms().saturating_sub(t0)
                        ),
                    )
                },
            ) {
                Ok(boot) => {
                    let (model, store, restored, pending) =
                        (boot.model, boot.shelf, boot.restored, boot.asks);
                    let how = boot.how.word();
                    log.line(
                        "[STORE] ",
                        format_args!(
                            "sectors={} used={} entries={} cursor={} torn={} foreign={} asks={} restore={how} restore_ms={}",
                            store.ring.sectors(),
                            store.used(),
                            restored.entries,
                            store.cursor(),
                            restored.torn,
                            restored.foreign,
                            pending.len(),
                            crate::uptime_ms().saturating_sub(t0)
                        ),
                    );
                    // Where the boot's time went (1521): per phase, in
                    // microseconds, and per 1000 entries decoded, in two
                    // lines of any length (1521c: one line outgrew
                    // `LINE_MAX` and lost its tail).
                    for line in boot.spent.lines(boot.how, boot.decoded) {
                        log.text("[STORE] ", &line);
                    }
                    // The summary a rebuild wrote before it returned
                    // (1603): the next boot resumes.
                    if boot.settle_calls > 0 {
                        log.line(
                            "[STORE] ",
                            format_args!(
                                "summary settled records={} writes={} erased={} calls={} left={}",
                                boot.settled.entries,
                                boot.settled.writes,
                                boot.settled.erased,
                                boot.settle_calls,
                                u8::from(store.owes())
                            ),
                        );
                    }
                    match boot.how {
                        How::Resumed { records, tail } => log.line(
                            "[STORE] ",
                            format_args!(
                                "index=summaries records={records} tail={tail} conversations={}",
                                model.conversations.len()
                            ),
                        ),
                        How::Rebuilt { why } => log.line(
                            "[STORE] ",
                            format_args!(
                                "index=rebuilt why={why} conversations={}",
                                model.conversations.len()
                            ),
                        ),
                        How::Whole => {}
                    }
                    // The asks the power cut off (1493): queued again,
                    // oldest first; their requests carry their own ids,
                    // so one that was answered goes without harm.
                    for ask in pending {
                        match board::ask_verb(ask.tag, &ask.request) {
                            Some(verb) => outgoing.push(ask.id, verb),
                            None => log.line("[HOME] ", format_args!("ask=unread tag={}", ask.tag)),
                        }
                    }
                    (model, Some(store))
                }
                Err(error) => {
                    log.line("[STORE] ", format_args!("restore=failed error={error}"));
                    (m9home::model(address), None)
                }
            },
        };
        let stored = store.as_ref().map_or(0, Shelf::cursor);
        let agreement = board::agree(&mut record, stored);
        if agreement != board::Agreement::Agrees {
            log.line(
                "[HOME] ",
                format_args!("{agreement} cursor={}", record.cursor),
            );
        }
        match record.device_identity() {
            Ok(device) => {
                // The home's LXMF address (1562): the bench's walker
                // refuses to open chats on a board joined to any home
                // but the bench's own.
                let mut home = [0u8; 32];
                for (i, byte) in address.iter().enumerate() {
                    home[2 * i] = hex(byte >> 4);
                    home[2 * i + 1] = hex(byte & 0x0f);
                }
                log.line(
                    "[HOME] ",
                    format_args!(
                        "client=yes admitted={} cursor={} page={} max_answer={} home={}",
                        if record.admitted() { "yes" } else { "no" },
                        record.cursor,
                        batch.page_bytes,
                        batch.max_answer,
                        core::str::from_utf8(&home).unwrap_or("?")
                    ),
                );
                let mut session = Session::new();
                if !outgoing.is_empty() {
                    session.hurry(crate::uptime_ms());
                }
                self.client = Some(Client {
                    record,
                    device,
                    address,
                    batch,
                    session,
                    exec: None,
                    sending: false,
                    pulled: 0,
                    progress: None,
                    outgoing,
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

    /// Entries the message store holds, for the `[LOOP]` line (1463); none
    /// on a board without a store or a home.
    pub fn entries(&self) -> u64 {
        self.client
            .as_ref()
            .and_then(|client| client.store.as_ref())
            .map_or(0, |shelf| shelf.ring.entries())
    }

    /// The reader's type size as the client record keeps it (1532,
    /// 1561): its two bits, which `loquax_m9::font::Size::of_record`
    /// names; none on a board that is no device of a home, which has no
    /// record to keep it in.
    pub fn type_size(&self) -> Option<u8> {
        self.client.as_ref().map(|client| client.record.type_size)
    }

    /// Keep the reader's type size in the client record (1532, 1561),
    /// through its two slots as a join's write. `Some(true)` kept,
    /// `Some(false)` the write failed (counted unsaved, which the screen
    /// says), `None` no record to keep it in. The record goes whole, with
    /// the cursor it holds in memory, which the boot agrees with the store
    /// anyway.
    pub fn keep_type(&mut self, size: u8, stores: &mut Stores<'_>) -> Option<bool> {
        let client = self.client.as_mut()?;
        if client.record.type_size == size {
            return Some(true);
        }
        client.record.type_size = size;
        let saved = stores.save_client(&client.record).is_ok();
        if !saved {
            client.drained.unsaved += 1;
        }
        Some(saved)
    }

    /// What the screen's top bar says of the home.
    pub fn word(&self) -> Option<Word> {
        self.client.as_ref().map(|client| {
            let admitted = client.record.admitted();
            Word::of(&client.session, admitted, client.progress)
        })
    }

    /// What a fold asked for (1493): every verb queued under a fresh id
    /// and kept in the message store, so a reboot queues it again; the
    /// marks among them kept as entries too, so read state survives the
    /// reboot. A write that fails counts as unsaved for the screen and
    /// is said in the log; the verb is still asked.
    pub fn ask(
        &mut self,
        verbs: Vec<Verb>,
        marks: &[Mark],
        ids: &mut impl FnMut() -> [u8; 16],
        now_ms: u64,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        if !marks.is_empty() {
            if let (Some(store), Some(mut dev)) = (client.store.as_mut(), stores.messages()) {
                match m9home::keep_marks(store, &mut dev, marks) {
                    // The ring's erase said (1542): a bench owe that
                    // closed its tail proves here that the mark's write
                    // opened the next sector.
                    Ok(done) => log.line(
                        "[STORE] ",
                        format_args!(
                            "marks={} writes={} erased={}",
                            done.entries, done.writes, done.erased
                        ),
                    ),
                    Err(error) => {
                        client.drained.unsaved += 1;
                        log.line("[STORE] ", format_args!("marks=unsaved error={error}"));
                    }
                }
            }
        }
        if verbs.is_empty() {
            return;
        }
        for verb in verbs {
            let id = ids();
            if let (Some(store), Some(mut dev)) = (client.store.as_mut(), stores.messages()) {
                if let Some((tag, bytes)) = board::ask_bytes(&verb) {
                    if let Err(error) = store.keep_ask(&mut dev, &id, tag, &bytes) {
                        client.drained.unsaved += 1;
                        log.line("[STORE] ", format_args!("ask=unsaved error={error}"));
                    }
                }
            }
            client.outgoing.push(id, verb);
        }
        client.session.hurry(now_ms);
    }

    /// The idle moment's flash work (1493): erase the message store's
    /// next ring sector while the line is quiet, so a pulled page writes
    /// into erased flash and never holds the interrupts for an erase.
    /// At most one erase per call; whether one was made.
    pub fn quiet(&mut self, stores: &mut Stores<'_>, log: &mut UartLog<'_>) -> bool {
        let Some(client) = self.client.as_mut() else {
            return false;
        };
        // What the summary log owes, a sector a quiet turn, a backlog's
        // next a second later (1591, `Shelf::quiet_flush`).
        client.quiet_flush(stores, log);
        let (Some(store), Some(mut dev)) = (client.store.as_mut(), stores.messages()) else {
            return false;
        };
        let erases = store.ring.erases();
        match store.quiet(&mut dev) {
            Ok(worked) => {
                if worked {
                    log.line(
                        "[STORE] ",
                        format_args!(
                            "pre_erased={} erases={}",
                            store.ring.erases() - erases,
                            store.ring.erases()
                        ),
                    );
                }
                worked
            }
            Err(error) => {
                log.line("[STORE] ", format_args!("pre_erase=failed error={error}"));
                false
            }
        }
    }

    /// The rows the model said anew, the conversations it deleted, and
    /// how many pulled pages it folded since (1521): into the summary
    /// log, and its state once every page kept has its rows.
    pub fn said(
        &mut self,
        rows: Vec<Row>,
        gone: Vec<[u8; 16]>,
        pages: u32,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) {
        let Some(client) = self.client.as_mut() else {
            return;
        };
        if let Some(store) = client.store.as_mut() {
            store.said(rows, gone);
            store.pages_owed = store.pages_owed.saturating_sub(pages);
        }
        client.flush(stores, log);
    }

    /// The next page of conversation `peer` the model has not loaded
    /// (1521): `want` entries at most from the conversation's own ring
    /// sectors, and how many older wait; `None` on a board without a
    /// store.
    pub fn load(
        &mut self,
        peer: [u8; 16],
        want: usize,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) -> Option<(Vec<Entry>, usize)> {
        let client = self.client.as_mut()?;
        let (Some(store), Some(mut dev)) = (client.store.as_mut(), stores.messages()) else {
            return None;
        };
        let t0 = crate::uptime_ms();
        match store.page(&mut dev, peer, want, &client.address) {
            Ok((entries, earlier)) => {
                let cost = store.index.last_page;
                log.line(
                    "[STORE] ",
                    format_args!(
                        "page_read entries={} messages={} marks={} sectors={} earlier={earlier} ms={}",
                        entries.len(),
                        cost.messages,
                        cost.marks,
                        cost.sectors,
                        crate::uptime_ms().saturating_sub(t0)
                    ),
                );
                Some((entries, earlier))
            }
            Err(error) => {
                log.line("[STORE] ", format_args!("page_read=failed error={error}"));
                None
            }
        }
    }

    /// What the last [`Handheld::load`] read (1556): the conversation's
    /// entries, the marks among them, the ring sectors.
    pub fn last_page(&self) -> loquax::home::board::rows::PageCost {
        self.client
            .as_ref()
            .and_then(|client| client.store.as_ref())
            .map(|store| store.index.last_page)
            .unwrap_or_default()
    }

    /// What the client has for the loop since the last call.
    pub fn drain(&mut self) -> Drained {
        let Some(client) = self.client.as_mut() else {
            return Drained::default();
        };
        let drained = core::mem::take(&mut client.drained);
        // The entries go to the model as one message; the summary's
        // state waits for the rows it says back (1521).
        if !drained.entries.is_empty() {
            if let Some(store) = client.store.as_mut() {
                store.pages_owed += 1;
            }
        }
        drained
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
                                store.ring.entries()
                            ),
                        ),
                        Err(error) => {
                            // A page the flash would not keep is on the
                            // screen now and gone after a reboot: a
                            // state, not a log line (1493).
                            client.drained.unsaved += 1;
                            log.line("[STORE] ", format_args!("kept=no error={error}"));
                        }
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

/// The bench's hooks into the store (1540), only in an image built for
/// the bench (`cfg(m9_bench)`): the race of 2026-11-07 needs a flash write
/// while the UI core folds an opened chat, and after the first open the
/// board owed the flash nothing, so the window was used up.
#[cfg(m9_bench)]
impl Handheld {
    /// Arm the owe with `flags` (`envelope::BENCH_OWE_*`); false on a
    /// board without a message store, which has nothing to owe.
    pub fn bench_owe(&mut self, flags: u8, log: &mut UartLog<'_>) -> bool {
        let stored = self
            .client
            .as_ref()
            .is_some_and(|client| client.store.is_some());
        if stored {
            self.owe = Some(flags);
        }
        log.line(
            "[BENCH] ",
            format_args!(
                "owe={} flags={flags:#04x}",
                if stored { "armed" } else { "refused" }
            ),
        );
        stored
    }

    /// Before the asks a fold sent are kept (the open's read mark among
    /// them): an armed owe that erases the ring closes its tail now, so
    /// the mark's write opens the next sector with an erase.
    pub fn bench_before_ask(&mut self, log: &mut UartLog<'_>) {
        let Some(flags) = self.owe else {
            return;
        };
        if flags & leviculum_core::envelope::BENCH_OWE_ERASE_RING == 0 {
            return;
        }
        if let Some(store) = self.client.as_mut().and_then(|c| c.store.as_mut()) {
            store.ring.close();
            self.owe = Some(flags & !leviculum_core::envelope::BENCH_OWE_ERASE_RING);
            log.line("[BENCH] ", format_args!("owe=ring-closed"));
        }
    }

    /// An open reached the I/O core, the UI core folding and drawing it
    /// now: an armed owe fires. Called on what every open sends (1542):
    /// the open's `Load` (the first open after a boot only), its read
    /// mark among the `Asks`, and its row read in `Rows`; an open in a
    /// running session sends no `Load`, so an owe that waited for one
    /// never fired. Every conversation's row and the ring's state with its
    /// marks are owed the summary log, the log's tail closed when asked,
    /// and written at once, while the other core allocates.
    pub fn bench_fire(&mut self, stores: &mut Stores<'_>, log: &mut UartLog<'_>) {
        let Some(flags) = self.owe.take() else {
            return;
        };
        let Some(client) = self.client.as_mut() else {
            return;
        };
        if let Some(store) = client.store.as_mut() {
            store.index.owe_all();
            store.dirty = true;
            if flags & leviculum_core::envelope::BENCH_OWE_ERASE_LOG != 0 {
                if let Some(rows) = store.log.as_mut() {
                    rows.close();
                }
            }
            // No ask came between the arm and the open: the ring's next
            // write erases, whichever it is.
            if flags & leviculum_core::envelope::BENCH_OWE_ERASE_RING != 0 {
                store.ring.close();
            }
        }
        log.line("[BENCH] ", format_args!("owe=fired flags={flags:#04x}"));
        client.flush(stores, log);
    }

    /// The bench's `n`th arrival (`TYPE_BENCH_ARRIVE`, 1562): from the
    /// bench's own peer into "Bench" (`loquax_m9::bench::entries`), kept
    /// in the ring and handed to the model as a pulled page is
    /// ([`Handheld::drain`]). False on a board that is no device of a
    /// home.
    pub fn bench_arrive(
        &mut self,
        arrival: loquax_m9::bench::Arrival,
        n: u32,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) -> bool {
        let Some(client) = self.client.as_mut() else {
            return false;
        };
        let seq = client.store.as_ref().map_or(0, |store| store.cursor());
        let entries = loquax_m9::bench::entries(client.address, arrival, n, seq);
        if let (Some(store), Some(mut dev)) = (client.store.as_mut(), stores.messages()) {
            match m9home::keep(store, &mut dev, &entries, &client.address) {
                Ok(done) => log.line(
                    "[STORE] ",
                    format_args!(
                        "kept={} writes={} erased={} cursor={} used={} entries={}",
                        done.entries,
                        done.writes,
                        done.erased,
                        store.cursor(),
                        store.used(),
                        store.ring.entries()
                    ),
                ),
                Err(error) => {
                    client.drained.unsaved += 1;
                    log.line("[STORE] ", format_args!("kept=no error={error}"));
                }
            }
        }
        client.drained.entries.extend(entries);
        true
    }

    /// After a forget (1562): the client made again from its record, the
    /// store restored from the flash (a rebuild, the summary being
    /// erased), and the model the UI core takes in place of its own.
    ///
    /// The old client goes before the rebuild starts (1603): its shelf,
    /// index and drained pages stood beside the new model for the
    /// rebuild's 25 s, and with the UI core's old model
    /// ([`Handheld::bench_release`]) a second whole model's worth of the
    /// IO pool was taken when the long chat's messages grew to 2048 of
    /// 136 B (window 22: `memory allocation of 278528 bytes failed`).
    pub fn bench_reopen(
        &mut self,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
        feed: &dyn Fn(),
    ) -> Option<UiModel<Field>> {
        // Moved out and the rest dropped here, not at the end of the call.
        let Client { record, batch, .. } = self.client.take()?;
        self.client(record, batch, stores, log, feed)
    }

    /// The empty model the UI core takes before a reopen (1603), so the
    /// one it shows, every message of the ring after a rebuild, is freed
    /// before the reopen builds the next: two whole models do not fit the
    /// IO pool beside the map and the renderer. `None` on a board that is
    /// no device of a home.
    pub fn bench_release(&self) -> Option<UiModel<Field>> {
        self.client
            .as_ref()
            .map(|client| m9home::model(client.address))
    }

    /// `TYPE_BENCH_FORGET` (1562): the bench's arrivals out of the
    /// message store and the summary log erased (`m9home::forget`), `feed`
    /// per sector for the watchdog. Whether the store must be opened
    /// again ([`Handheld::bench_reopen`]): the one in memory no longer
    /// stands for the flash. False, and a refusal said, on a board with
    /// no home or no store.
    pub fn bench_forget(
        &mut self,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
        feed: &dyn Fn(),
    ) -> bool {
        let Some(client) = self.client.as_ref() else {
            log.line("[BENCH] ", format_args!("forget=refused reason=no-home"));
            return false;
        };
        let Some(mut dev) = stores.messages() else {
            log.line("[BENCH] ", format_args!("forget=refused reason=no-store"));
            return false;
        };
        let t0 = crate::uptime_ms();
        match m9home::forget(&mut dev, client.address, feed) {
            Ok(done) => {
                log.line(
                    "[BENCH] ",
                    format_args!(
                        "forget=done flagged={} unflagged={} marks={} entries={} frames={} sectors={} summary_erased={} ms={}",
                        done.flagged,
                        done.unflagged,
                        done.marks,
                        done.store.entries,
                        done.store.frames,
                        done.store.sectors,
                        done.summary_erased,
                        crate::uptime_ms().saturating_sub(t0)
                    ),
                );
                true
            }
            Err(error) => {
                // Sectors may be written again already: open the store
                // again all the same, from what the flash holds.
                log.line("[BENCH] ", format_args!("forget=failed error={error:?}"));
                true
            }
        }
    }
}

impl Client {
    /// Write what the summary log is owed (1521): the rows the model
    /// said, and the ring's state once every page kept has its rows. One
    /// bounded write a call (1591, 1597, `Shelf::flush`): what is left
    /// goes in the quiet turns ([`Client::quiet_flush`]), the UI core
    /// unparked between.
    fn flush(&mut self, stores: &mut Stores<'_>, log: &mut UartLog<'_>) {
        self.flush_with(stores, log, None);
    }

    /// [`Client::flush`] in a quiet turn: a backlog's next write not
    /// before `m9home::BACKLOG_GAP_MS` after the last.
    fn quiet_flush(&mut self, stores: &mut Stores<'_>, log: &mut UartLog<'_>) {
        self.flush_with(stores, log, Some(crate::uptime_ms()));
    }

    fn flush_with(&mut self, stores: &mut Stores<'_>, log: &mut UartLog<'_>, quiet: Option<u64>) {
        let (Some(store), Some(mut dev)) = (self.store.as_mut(), stores.messages()) else {
            return;
        };
        let outgoing = &self.outgoing;
        let written = match quiet {
            Some(now_ms) => store.quiet_flush(&mut dev, || outgoing.pending(), now_ms),
            None => store.flush(&mut dev, || outgoing.pending()),
        };
        match written {
            Ok(wrote) => {
                let done = wrote.appended();
                if done.writes > 0 {
                    log.line(
                        "[STORE] ",
                        format_args!(
                            "summary records={} writes={} erased={} left={}",
                            done.entries,
                            done.writes,
                            done.erased,
                            u8::from(wrote.partial())
                        ),
                    );
                }
            }
            Err(error) => {
                // The next boot finds the ring past its summary and reads
                // it whole: slow, never wrong.
                log.line("[STORE] ", format_args!("summary=unsaved error={error}"));
            }
        }
    }

    /// Begin the ask the record owes: its join while pending; once
    /// admitted the oldest queued verb, else a pull.
    fn start(&mut self, now_ms: u64, log: &mut UartLog<'_>) {
        self.sending = false;
        let ask = match (&self.record.admission, self.outgoing.next()) {
            (Admission::Pending { .. }, _) => self.record.join(now_ms, JOIN_BUDGET_MS),
            (Admission::Admitted, Some((_, verb))) => {
                self.sending = true;
                Some(Ask::new(
                    &self.record.home,
                    verb.clone(),
                    now_ms,
                    SEND_BUDGET_MS,
                ))
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

    /// The oldest verb is over, taken or refused for good: pop it, put
    /// its done frame behind it and hand the model its answer.
    fn finish_ask(
        &mut self,
        result: &Result<Outcome, HomeError>,
        stores: &mut Stores<'_>,
        log: &mut UartLog<'_>,
    ) {
        let Some((id, verb)) = self.outgoing.done() else {
            return;
        };
        if let (Some(store), Some(mut dev)) = (self.store.as_mut(), stores.messages()) {
            if let Err(error) = store.ask_done(&mut dev, &id) {
                self.drained.unsaved += 1;
                log.line("[STORE] ", format_args!("ask_done=unsaved error={error}"));
            }
        }
        if let Some(answer) = m9home::answer_of(&verb, result) {
            self.drained.answers.push(answer);
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
                if saved.is_err() {
                    // The board is admitted but the record is not in
                    // flash: a reboot lands on the pairing screen with a
                    // used token. A word on the screen, not only here
                    // (1493).
                    self.drained.join_unsaved = true;
                }
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
                // The cursor lives in the message store, which kept every
                // page as it came; the record is not written (1463).
                self.record.cursor = self.record.cursor.max(*cursor);
                log.line(
                    "[HOME] ",
                    format_args!(
                        "pulled entries={entries} pages={pages} cursor={cursor} saved=store reach={reach} next_in_ms={next_in}"
                    ),
                );
            }
            Ok(outcome) if self.sending => {
                // The oldest verb ended: the home took it. Its done
                // frame goes behind it, so a reboot does not ask again,
                // and its answer goes to the model.
                self.finish_ask(result, stores, log);
                // What the home archived for it comes with the next
                // pull, which names the bubble on every device.
                self.session.hurry(now_ms);
                log.line(
                    "[HOME] ",
                    format_args!(
                        "asked done={outcome:?} waiting={} reach={reach}",
                        self.outgoing.len()
                    ),
                );
            }
            Ok(other) => log.line("[HOME] ", format_args!("done={other:?} reach={reach}")),
            Err(error) if self.sending => {
                // A refusal stays one; anything else is asked again when
                // the session says, the verb kept at the front.
                if matches!(error, HomeError::Refused(_)) {
                    self.finish_ask(result, stores, log);
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
                log.line(
                    "[HOME] ",
                    format_args!(
                        "failed error=\"{error}\" pulled={} cursor={} saved=store reach={reach} failures={} next_in_ms={next_in}",
                        self.pulled,
                        self.record.cursor,
                        self.session.failures()
                    ),
                );
            }
        }
    }
}
