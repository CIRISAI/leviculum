//! The messenger's shell below its frontends: the engine attached to the
//! running daemon, the history, the job queue, the mailbox's schedule, and
//! the model ([`loquax::ui`]) they feed. The terminal (`tui`) and the
//! window (`gui`) each start one with [`start`], fold their reader's events
//! into it, draw what it holds and hand it back to [`finish`]; neither
//! wires an engine, a store or a runner of its own.
//!
//! - [`Shell`] holds the engine, the fan-out and the job queue: it folds
//!   events into the model and runs the effects that come back. A send is
//!   [`run_send_auto`] and a fetch is [`run_fetch`], the CLI's own runners,
//!   each on a [`Tap`]: sends one at a time, and a fetch alongside them, so
//!   a send never waits for a mailbox that does not answer (leviculum 1072).
//!   The screen reads the same events.
//! - [`start`] is everything before the first frame: the identity, the
//!   store and what it restores, the daemon, the contacts, the schedule and
//!   the map's regions. [`finish`] lets the running jobs settle and detaches.
//!
//! Moved out of `tui.rs` when the GUI came (leviculum 1084): what is here
//! was the terminal's and is now both frontends'.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use leviculum_lxmf::Field;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tui_input::Input;

use loquax::clock::Tokio;
use loquax::effects::{Effects, Terminal as Files};
use loquax::engine::{attach_as, AttachConfig, Attached, Role};
use loquax::fanout::{Fanout, Tap};
use loquax::fetch::{run_fetch, FetchError, FetchOptions};
use loquax::home::runner::{self, HomeClient};
use loquax::home::watch::{self, Fallback, WatchEvent};
use loquax::mailbox::{Unanswered, UNANSWERED_EXCLUSION_MS, UNANSWERED_LIMIT};
use loquax::mailbox_state::{self, short, Stored};
use loquax::mapload::{Loader, Source, Step, CACHE_DEFAULT_MB};
use loquax::outbox::{Command, Outbox, OutboxEvent, OutboxGone, SendRequest, Via};
use loquax::picture::Maps;
use loquax::seen::{SeenStore, SeenWrite};
use loquax::send::{run_send_auto, AutoError, Leg, SendError, SendOptions};
use loquax::store::Store;
use loquax::ui::history::StoreRead;
use loquax::ui::map::{MapSetup, MapWant};
use loquax::ui::{update, MapLoad, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;
use loquax::{display_name, identity};

/// The model as both frontends hold it: its text fields are `tui-input`'s,
/// a buffer without a terminal in it.
pub(crate) type Model = UiModel<Input>;

/// The spinner and toast cadence the brief names.
pub(crate) const TICK_MS: u64 = 250;
/// How often the engine's queue is read: the send runner's own poll.
pub(crate) const PUMP_MS: u64 = 20;
/// How long leaving waits for the running send or fetch to read what the
/// engine already said. A runner reads its tap every 20 ms, so one with its
/// final state in hand ends well inside this; one still waiting on the
/// network is dropped after it, as before.
const SETTLE_MS: u64 = 1_000;
/// A key script's time of day until its `clock` step says another:
/// Monday 26 October 2026, 12:00, at UTC. A script never reads the wall
/// clock, so its frames are the same on every run.
const SCRIPT_CLOCK: (i64, u32, u32, u32, u32) = (2026, 10, 26, 12, 0);

/// The log file's filter without `RUST_LOG`.
const LOG_FILTER: &str = "warn,loquax=info";

/// What a frontend tells the shell it starts.
pub(crate) struct ShellOptions {
    /// `--sync-interval`, when given: the schedule's bound in milliseconds,
    /// `None` inside for the timer off.
    pub sync_interval: Option<Option<u64>>,
    pub to: Vec<[u8; 16]>,
    pub pn: Option<String>,
    pub instance: Option<String>,
    pub config: Option<PathBuf>,
    pub timeout: u64,
    pub ascii: bool,
    pub no_mouse: bool,
    /// A key script drives the session: its own clock, and a mailbox
    /// schedule only when the command line names one.
    pub scripted: bool,
    /// The file beside the identity the log goes to: `tui.log`, `gui.log`.
    pub log_file: &'static str,
}

/// Effects for the runners and the stamp worker: their lines become toasts,
/// their files are the CLI's, and a fetched message is not printed, because
/// the screen already has it from the engine's `Received`.
#[derive(Clone)]
pub(crate) struct ScreenEffects {
    notices: UnboundedSender<String>,
    /// The send runner's leg changes, for the running send's row.
    legs: UnboundedSender<Leg>,
    /// Each selection a runner keeps, for the header's mailbox state.
    mailbox: UnboundedSender<Stored>,
    files: PathBuf,
}

impl Effects for ScreenEffects {
    fn notice(&mut self, line: &str) {
        let _ = self.notices.send(line.to_string());
    }

    fn leg(&mut self, leg: Leg) {
        let _ = self.legs.send(leg);
    }

    fn print(&mut self, _rendered: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn save_mailbox(&mut self, stored: &Stored) -> Result<(), String> {
        let _ = self.mailbox.send(*stored);
        Files::new(self.files.clone()).save_mailbox(stored)
    }

    fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String> {
        Files::new(self.files.clone()).write_seen(write)
    }

    fn unix_now_ms(&self) -> u64 {
        loquax::mailbox_state::unix_now_ms()
    }
}

/// What the runners need besides a tap.
#[derive(Clone)]
pub(crate) struct RunContext {
    home: PathBuf,
    config_dir: PathBuf,
    instance: String,
    preferred_pn: Option<[u8; 16]>,
    pn_source: &'static str,
    timeout: Duration,
    effects: ScreenEffects,
    /// The nodes that stopped answering when the job started, for its
    /// mailbox selection.
    excluded: Vec<[u8; 16]>,
    /// Set when this device is a client of a home node: a fetch pulls from
    /// the home node, and a send is refused until sending through it exists.
    client: Option<HomeClient>,
}

/// A runner that ended, as its task hands it to the shell.
enum Finished {
    /// The running send, as the event the model folds.
    Send(UiEvent),
    /// The running fetch.
    Fetch(FetchEnd),
    /// Another runner's answer, as the event the model folds (1307).
    Event(UiEvent),
}

/// How a fetch ended, for the screen, `tui.log` and the mailbox's streak.
struct FetchEnd {
    result: Result<usize, FetchError>,
    /// The node the drain was pointed at: the first selection it saw.
    node: Option<[u8; 16]>,
    elapsed_ms: u64,
    /// On a client of a home node: the cursor to save, once the events the
    /// pull caused are in the store ([`loquax::home::client::Pull::cursor`]).
    cursor: Option<u64>,
    /// On a client of a home node: the imported history rows the pull wrote
    /// to the store ([`runner::PullEnd::imported`]).
    imported: usize,
}

/// A runner's tap that notes the first node a selection named.
struct Watched {
    tap: Tap,
    node: Option<[u8; 16]>,
}

impl Outbox for Watched {
    fn submit(&self, command: Command) -> Result<(), OutboxGone> {
        self.tap.submit(command)
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
        let event = self.tap.try_next_event()?;
        if let Some(OutboxEvent::PnSelected { destination, .. }) = &event {
            self.node.get_or_insert(*destination);
        }
        Ok(event)
    }
}

/// The engine, the fan-out, the job queue, the store, and the model they
/// feed.
pub(crate) struct Shell {
    pub(crate) model: Model,
    /// Held while the messenger runs: no home node on this home meanwhile.
    _delivery_lock: Option<loquax::home::lock::DeliveryLock>,
    /// The history: every fold's changes are written here before the next
    /// event, and pages and search results are read from it.
    store: Store,
    /// The store failed a write: said once, then on every later failure in
    /// the log.
    store_failed: bool,
    attached: Attached,
    fanout: Fanout,
    context: RunContext,
    /// Sends not yet started, oldest first. They run one at a time: the
    /// model names a queued message after the oldest running send.
    sends: VecDeque<([u8; 16], String, Vec<Field>)>,
    /// A fetch asked for and not yet started, with its reason's word. The
    /// model asks for one at a time.
    fetch_owed: Option<&'static str>,
    /// The running send's task, so leaving can let it finish its last poll.
    send_task: Option<tokio::task::JoinHandle<()>>,
    /// The running fetch's task and its reason's word.
    fetch_task: Option<(tokio::task::JoinHandle<()>, &'static str)>,
    /// The tasks handing marks to the home node, waited for before leaving.
    mark_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// The mailbox's syncs that failed before a link, and the nodes they
    /// excluded.
    unanswered: Unanswered,
    /// A node just excluded, named beside the node that replaces it.
    replacing: Option<[u8; 16]>,
    /// Every leg a runner reported, with the message it was for (the
    /// model's conversation and message index): a script's `until leg`.
    pub(crate) legs_seen: Vec<((usize, usize), Leg)>,
    done_tx: UnboundedSender<Finished>,
    done_rx: UnboundedReceiver<Finished>,
    notices_rx: UnboundedReceiver<String>,
    legs_rx: UnboundedReceiver<Leg>,
    mailbox_rx: UnboundedReceiver<Stored>,
    /// Lines to print once the terminal is restored.
    pub(crate) afterwards: Vec<String>,
    /// The engine stopped answering.
    pub(crate) gone: bool,
    /// The reader asked for the screen to be drawn again from scratch.
    pub(crate) redraw: bool,
    /// What the screen asked of the terminal and not yet done: the real
    /// terminal does it, a script writes it into its frames.
    pub(crate) outputs: Vec<Output>,
    /// The maps directory every painter of this session draws from.
    pub(crate) maps: Maps,
    /// The autoload: regions the model asks for, one load at a time.
    loader: Loader,
    /// The origin of the monotonic clock the mailbox schedule runs on.
    origin: tokio::time::Instant,
    /// On a client of a home node: the watching link's task, its word on
    /// wakes and on itself, whether it is up, and the pull while it is
    /// down. A script runs none, as it runs no schedule unless it says so.
    watch: Option<(tokio::task::JoinHandle<()>, UnboundedReceiver<WatchEvent>)>,
    watching: bool,
    fallback: Fallback,
    /// The desktop's microphone (1276): the system's recorder, run on the
    /// model's recording effects and pumped on every poll.
    mic: crate::mic::Mic,
}

/// What the model asks of the terminal beyond drawing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Output {
    /// Put the text on the clipboard.
    Copy(String),
    /// Hand the URL to the system.
    Open(String),
    /// Capture the mouse or let it go.
    Mouse(bool),
    /// Draw the map the `n`th way the frontend offered (1093).
    MapDrawing(usize),
    /// Show this photo's file whole in the system's viewer (1220).
    Photo(std::path::PathBuf),
    /// Open this received file with the system's handler for its kind
    /// (1221).
    File(std::path::PathBuf),
}

impl Shell {
    /// The loquax home: the identity, the store, the config. The terminal
    /// writes its map drawing there.
    #[cfg(feature = "tui")]
    pub(crate) fn home(&self) -> &std::path::Path {
        &self.context.home
    }

    /// Fold one event, write what it changed to the store, and run what
    /// comes back.
    pub(crate) fn apply(&mut self, event: UiEvent) {
        let effects = update(&mut self.model, event);
        self.persist();
        let mut answers = Vec::new();
        for effect in effects {
            match effect {
                UiEffect::Store(read) => answers.extend(self.read(read)),
                // A terminal attached to a daemon dials nothing itself; a
                // leaf's rotation hears it (1298).
                UiEffect::Network { kind, changed } => {
                    self.attached.network().say(kind, changed);
                }
                UiEffect::Send { to, body, fields } => {
                    self.log_receipt(&to, &body, &fields);
                    self.sends.push_back((to, body, fields));
                }
                UiEffect::Fetch => {
                    let reason = self.model.fetch_reason.map_or("key", |r| r.word());
                    tracing::info!("SYNC reason={reason}");
                    self.fetch_owed = Some(reason);
                }
                UiEffect::Quit => {}
                UiEffect::Redraw => self.redraw = true,
                UiEffect::Notice(line) => self.afterwards.push(line),
                UiEffect::Copy(text) => self.outputs.push(Output::Copy(text)),
                UiEffect::Open(url) => self.outputs.push(Output::Open(url)),
                UiEffect::Mouse(on) => self.outputs.push(Output::Mouse(on)),
                UiEffect::MapDrawing(n) => self.outputs.push(Output::MapDrawing(n)),
                // The screen already shows the change; a file that would not
                // take it is said, because the next start would not show it.
                UiEffect::Contact(contact) => {
                    if let Err(error) = loquax::names::save_contact(&self.context.home, &contact) {
                        let line = format!(
                            "{}: not saved: {error}",
                            self.context
                                .home
                                .join(loquax::names::CONTACTS_FILE)
                                .display()
                        );
                        tracing::warn!("{line}");
                        self.afterwards.push(line.clone());
                        answers.push(UiEvent::Notice(line));
                    }
                }
                UiEffect::Log(line) => tracing::warn!("{line}"),
                UiEffect::LoadMap(want) => self.map_ask(want),
                UiEffect::Import => answers.extend(self.import()),
                // A photo whole (1220): the terminal has no pixels for it,
                // so the system's viewer shows it, from a file under the
                // home as the phone keeps it in its store.
                UiEffect::ShowPhoto { peer, key } => match self.photo_file(peer, key) {
                    Ok(path) => self.outputs.push(Output::Photo(path)),
                    Err(line) => answers.push(UiEvent::Notice(line)),
                },
                // A file (1221): saved under the home's files/, as the
                // phone keeps it under its app files, then the system's
                // handler opens it.
                UiEffect::OpenFile { peer, key, index } => {
                    let saved = self
                        .model
                        .conversations
                        .iter()
                        .find(|c| c.peer == peer)
                        .and_then(|c| c.messages.iter().find(|m| m.activity == key))
                        .ok_or_else(|| "that message is gone".to_string())
                        .and_then(|m| {
                            loquax::file_save::save(&self.context.home, peer, key, index, &m.fields)
                        });
                    match saved {
                        Ok(path) => self.outputs.push(Output::File(path)),
                        Err(line) => answers.push(UiEvent::Notice(line)),
                    }
                }
                // A voice message (1222): decoded into a WAV file under the
                // home's voice/, which the system's player plays; the model
                // hears of no end, so the bubble's button goes back at once.
                UiEffect::PlayVoice { peer, key } => {
                    #[cfg(feature = "voice")]
                    let saved = self
                        .model
                        .conversations
                        .iter()
                        .find(|c| c.peer == peer)
                        .and_then(|c| c.messages.iter().find(|m| m.activity == key))
                        .ok_or_else(|| "that message is gone".to_string())
                        .and_then(|m| {
                            loquax::voice::save_wav(&self.context.home, peer, key, &m.fields)
                        });
                    #[cfg(not(feature = "voice"))]
                    let saved: Result<std::path::PathBuf, String> =
                        Err("this build plays no voice messages".to_string());
                    answers.push(UiEvent::VoiceEnded { peer, key });
                    match saved {
                        Ok(path) => self.outputs.push(Output::File(path)),
                        Err(line) => answers.push(UiEvent::Notice(line)),
                    }
                }
                UiEffect::PauseVoice { .. } => {}
                // A group (1307): the home makes it, so a terminal with no
                // home says so at once.
                UiEffect::CreateGroup { name, members } => match self.context.client.clone() {
                    Some(client) => {
                        let done = self.done_tx.clone();
                        let budget = self.context.timeout;
                        self.mark_tasks.push(tokio::spawn(async move {
                            let event = runner::create_group(client, name, members, budget).await;
                            let _ = done.send(Finished::Event(event));
                        }));
                    }
                    None => answers.push(UiEvent::GroupMade(Err(
                        "groups live at a mailbox, and this Loquax has none".to_string(),
                    ))),
                },
                UiEffect::AddMembers { group, members } => match self.context.client.clone() {
                    Some(client) => {
                        let done = self.done_tx.clone();
                        let budget = self.context.timeout;
                        self.mark_tasks.push(tokio::spawn(async move {
                            let event = runner::add_members(client, group, members, budget).await;
                            let _ = done.send(Finished::Event(event));
                        }));
                    }
                    None => answers.push(UiEvent::MembersAdded(Err(
                        "groups live at a mailbox, and this Loquax has none".to_string(),
                    ))),
                },
                // The recording bar's effects (1276): the microphone runs
                // them; a finished recording comes back as its voice
                // message, a preview as a WAV file for the system's player.
                UiEffect::Recording { peer, effect } => {
                    let done = self.mic.effect(peer, effect, &self.context.home);
                    if let Some(path) = done.play {
                        self.outputs.push(Output::File(path));
                    }
                    answers.extend(done.events);
                }
                // A setting changed on screen (1233): the next start reads
                // it; a file that would not take it is said.
                UiEffect::Remember { key, value } => {
                    if let Err(error) = loquax::config::save(&self.context.home, key, value) {
                        let line = format!(
                            "{}: {key} not saved: {error}",
                            self.context
                                .home
                                .join(loquax::config::CONFIG_FILE)
                                .display()
                        );
                        tracing::warn!("{line}");
                        self.afterwards.push(line.clone());
                        answers.push(UiEvent::Notice(line));
                    }
                }
            }
        }
        for answer in answers {
            self.apply(answer);
        }
        self.start_next();
    }

    /// The photo of the message `key` of `peer`'s conversation as a file
    /// under the home's `photos/`, named by the message, written once.
    fn photo_file(&self, peer: [u8; 16], key: u64) -> Result<std::path::PathBuf, String> {
        let (format, data) = self
            .model
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .and_then(|c| c.messages.iter().find(|m| m.activity == key))
            .and_then(|m| {
                let (format, data) = loquax::ui::photos::photo_parts(&m.fields)?;
                Some((format, data.to_vec()))
            })
            .ok_or_else(|| "that message has no photo".to_string())?;
        let ext: String = format
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(5)
            .collect();
        let dir = self.context.home.join("photos");
        let path = dir.join(format!("{}-{key}.{ext}", loquax::address::to_hex(&peer)));
        if !path.exists() {
            std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::write(&path, &data))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(path)
    }

    /// A read receipt among a send's fields: one `tui.log` line, alone or
    /// on a message, with the monotonic clock it left at (1231).
    fn log_receipt(&self, to: &[u8; 16], body: &str, fields: &[Field]) {
        let Some(receipt) = leviculum_lxmf::lev::Lev::from_fields(fields).and_then(|l| l.receipt)
        else {
            return;
        };
        let alone = body.is_empty() && fields.len() == 2;
        tracing::info!(
            "RECEIPT send to={} ids={} read_at={} alone={} elapsed_ms={}",
            short(to),
            receipt.ids.len(),
            receipt.read_at,
            alone,
            self.elapsed_ms()
        );
    }

    /// The palette's `import`: contacts and history from every messenger
    /// found in its default place, never the identity (`crate::import`).
    /// What it did is said on screen and kept for after it; what it wrote
    /// shows at once (1162): the model takes the contacts file again and the
    /// store's history as a pull's imported rows ([`Self::show_imported`]).
    fn import(&mut self) -> Vec<UiEvent> {
        let Some(user_home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return vec![UiEvent::Notice("import: HOME is not set".to_string())];
        };
        let found = loquax::import::scan(&user_home, true);
        let trees: Vec<_> = found
            .iter()
            .filter(|f| matches!(f.source, loquax::import::Source::NomadNet(_)))
            .collect();
        if trees.is_empty() {
            return vec![UiEvent::Notice(
                "import: no history found; `loquax import <path>` reads a file or directory"
                    .to_string(),
            )];
        }
        let choices = loquax::import::Choices {
            contacts: true,
            history: true,
            identity: false,
        };
        let mut out = Vec::new();
        let mut imported = 0;
        for source in trees {
            let lines = match loquax::import::import(source, &self.context.home, choices, false) {
                Ok(report) => {
                    imported += report.conversations.iter().map(|c| c.new).sum::<usize>();
                    let mut lines = vec![format!("imported from {}", source.path.display())];
                    lines.extend(report.lines(false));
                    lines.push("the identity: `loquax import`".to_string());
                    lines
                }
                Err(error) => vec![format!("import: {error}")],
            };
            self.afterwards.extend(lines.iter().cloned());
            out.push(UiEvent::Notice(lines.join("; ")));
        }
        match loquax::names::load_contacts(&self.context.home) {
            Ok(contacts) => self.model.names.contacts = contacts,
            Err(error) => tracing::warn!("IMPORT contacts not shown: {error}"),
        }
        self.show_imported(imported);
        out
    }

    /// The fold's changes, in one transaction. A failed write is said on
    /// screen and kept for after the screen: the message is still shown,
    /// but the reader must know it will not be there after a restart.
    fn persist(&mut self) {
        self.hand_marks();
        self.hand_petnames();
        self.hand_peer_states();
        let writes = self.model.take_writes();
        if let Err(error) = self.store.write(&writes) {
            loquax::events::store_failed(&error.to_string());
            if !self.store_failed {
                self.store_failed = true;
                self.afterwards.push(error.to_string());
            }
            update(
                &mut self.model,
                UiEvent::Notice(format!("not saved: {error}")),
            );
        }
    }

    /// The petnames the reader set, handed to the home node in order on a
    /// task of their own, as marks are (1139).
    fn hand_petnames(&mut self) {
        let petnames = self.model.take_petnames();
        let Some(client) = self.context.client.clone() else {
            return;
        };
        if petnames.is_empty() {
            return;
        }
        self.mark_tasks.retain(|task| !task.is_finished());
        let budget = self.context.timeout;
        self.mark_tasks.push(tokio::spawn(runner::hand_petnames(
            client, petnames, budget,
        )));
    }

    /// The peer states the reader set (1146), handed to the home node as
    /// petnames are.
    fn hand_peer_states(&mut self) {
        let states = self.model.take_peer_states();
        let Some(client) = self.context.client.clone() else {
            return;
        };
        if states.is_empty() {
            return;
        }
        self.mark_tasks.retain(|task| !task.is_finished());
        let budget = self.context.timeout;
        self.mark_tasks.push(tokio::spawn(runner::hand_peer_states(
            client, states, budget,
        )));
    }

    /// The marks a fold made, handed to the home node in order, on a task
    /// of their own: the screen already shows them. A mark the home node did
    /// not take is said in `tui.log`; the other devices keep the message
    /// unread until this one marks it again.
    fn hand_marks(&mut self) {
        let marks = self.model.take_marks();
        let Some(client) = self.context.client.clone() else {
            return;
        };
        if marks.is_empty() {
            return;
        }
        self.mark_tasks.retain(|task| !task.is_finished());
        let budget = self.context.timeout;
        self.mark_tasks
            .push(tokio::spawn(runner::hand_marks(client, marks, budget)));
    }

    /// What the screen asked the store, answered as the event the model
    /// waits for.
    fn read(&mut self, read: StoreRead) -> Option<UiEvent> {
        let answer = match read {
            StoreRead::Earlier {
                peer,
                before,
                through,
            } => self
                .store
                .earlier(peer, before, through)
                .map(|(messages, earlier)| UiEvent::Earlier {
                    peer,
                    messages,
                    earlier,
                }),
            StoreRead::Search(query) => self
                .store
                .search(&query, &self.model.loaded_from())
                .map(|hits| UiEvent::StoredHits { query, hits }),
        };
        match answer {
            Ok(event) => Some(event),
            Err(error) => {
                loquax::events::store_failed(&error.to_string());
                self.afterwards.push(error.to_string());
                Some(UiEvent::Notice(format!("history: {error}")))
            }
        }
    }

    /// Everything that arrived since the last call, without waiting.
    pub(crate) fn poll(&mut self) {
        match self.fanout.pump(&mut self.attached.outbox) {
            Ok(events) => {
                for event in events {
                    let named = match &event {
                        OutboxEvent::Announced {
                            destination,
                            name: Some(_),
                        } => Some(*destination),
                        _ => None,
                    };
                    if runner::already_held(&self.store, &event) {
                        continue;
                    }
                    runner::keep_signature(&mut self.store, &event);
                    self.apply(UiEvent::Engine(event));
                    if let Some(destination) = named {
                        runner::keep_name(
                            &mut self.store,
                            &self.model.names,
                            self.model.address,
                            destination,
                        );
                    }
                }
            }
            Err(gone) => {
                if !self.gone {
                    self.gone = true;
                    self.apply(UiEvent::Notice(gone.to_string()));
                    self.afterwards.push(gone.to_string());
                }
            }
        }
        // Before the runner's end: a leg it moved onto belongs to the send
        // that is still at the front of the model's queue.
        while let Ok(leg) = self.legs_rx.try_recv() {
            if let Some(&message) = self.model.sending.front() {
                self.legs_seen.push((message, leg));
            }
            self.apply(UiEvent::SendLeg(leg));
        }
        while let Ok(finished) = self.done_rx.try_recv() {
            match finished {
                Finished::Send(event) => {
                    self.send_task = None;
                    self.apply(event);
                }
                Finished::Fetch(end) => self.fetch_done(end),
                Finished::Event(event) => self.apply(event),
            }
        }
        while let Ok(line) = self.notices_rx.try_recv() {
            self.apply(UiEvent::Notice(line));
        }
        self.watched();
        while let Ok(stored) = self.mailbox_rx.try_recv() {
            self.replaced(&stored);
            self.apply(chosen(&stored));
        }
        let steps = self.loader.poll(self.elapsed_ms());
        self.map_steps(steps);
        self.apply(UiEvent::Elapsed(self.elapsed_ms()));
        self.listen();
    }

    /// The microphone's windows since the last poll, as levels, and the
    /// recording's clock (1276); a recorder that stopped by itself is
    /// said once.
    fn listen(&mut self) {
        let pumped = self.mic.pump(self.elapsed_ms());
        for input in pumped.inputs {
            self.apply(UiEvent::Record {
                peer: pumped.peer,
                input,
            });
        }
        if let Some(line) = pumped.stopped {
            self.apply(UiEvent::Notice(line));
        }
    }

    /// A gesture on the window's microphone (1276), on the shell's clock:
    /// the window has none of its own that the model's recording counts
    /// by.
    #[cfg(feature = "gui")]
    pub(crate) fn record(&mut self, peer: [u8; 16], input: loquax::ui::recording::Input) {
        use loquax::ui::recording::Input;
        let now = self.elapsed_ms();
        let input = match input {
            Input::Press { .. } => Input::Press { now },
            Input::Release { .. } => Input::Release { now },
            Input::Tick { .. } => Input::Tick { now },
            other => other,
        };
        self.apply(UiEvent::Elapsed(now));
        self.apply(UiEvent::Record { peer, input });
    }

    /// The clock's tick, every [`TICK_MS`]: the time of day to the model,
    /// a suspend said and folded where the wall clock ran ahead of the
    /// monotonic one (`clocks` holds both as last read), then the
    /// spinner's tick.
    pub(crate) fn tick(&mut self, clocks: &mut (i64, u64)) {
        let now = local_now();
        let read = (now.unix_secs, self.elapsed_ms());
        self.apply(UiEvent::Clock(now));
        if loquax::sync::resumed(*clocks, read) {
            tracing::info!("SYNC resumed after {}s", read.0 - clocks.0);
            self.apply(UiEvent::Resumed);
        }
        *clocks = read;
        self.apply(UiEvent::Tick);
    }

    /// The schedule's clock: tokio's, which on Linux stands still in
    /// suspend, as `crate::sync::resumed` needs.
    pub(crate) fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Start the oldest send if no send is running, and the owed fetch if no
    /// fetch is. A send and a fetch run alongside: each runner reads every
    /// event and acts only on its own (a send on its message id, a fetch on
    /// the drain's `Sync*` events), and both select the mailbox by the same
    /// rule from the same input, so a selection either asks for is the one
    /// the other uses. Sends stay one at a time, because the model names a
    /// queued message after the oldest running send.
    fn start_next(&mut self) {
        if self.send_task.is_none() {
            if let Some((to, body, fields)) = self.sends.pop_front() {
                let tap = self.fanout.tap();
                let context = self.job_context();
                let done = self.done_tx.clone();
                self.send_task = Some(tokio::spawn(async move {
                    let event = send(tap, context, to, body, fields).await;
                    let _ = done.send(Finished::Send(event));
                }));
            }
        }
        if self.fetch_task.is_none() {
            if let Some(reason) = self.fetch_owed.take() {
                let tap = self.fanout.tap();
                let context = self.job_context();
                let done = self.done_tx.clone();
                let task = tokio::spawn(async move {
                    let end = fetch(tap, context).await;
                    let _ = done.send(Finished::Fetch(end));
                });
                self.fetch_task = Some((task, reason));
            }
        }
    }

    /// Load `want`'s region, from the model's [`UiEffect::LoadMap`] or the
    /// window's key: queued behind a running load, skipped while its box
    /// backs off.
    pub(crate) fn map_ask(&mut self, want: MapWant) {
        let steps = self.loader.ask(want, self.elapsed_ms());
        self.map_steps(steps);
    }

    /// The loader's steps: one `tui.log` line each, and the model told. A
    /// region that landed reaches the model's setup before its `Done`, so
    /// the fit that follows sees it.
    fn map_steps(&mut self, steps: Vec<Step>) {
        for step in steps {
            match step {
                Step::Started(want) => {
                    tracing::info!(
                        "MAPLOAD start box={} zooms={}-{} reason={}",
                        box_words(&want),
                        want.zooms.0,
                        want.zooms.1,
                        want.reason.word()
                    );
                    self.apply(UiEvent::MapLoad(MapLoad::Started(want)));
                }
                Step::Done {
                    want,
                    landed,
                    elapsed_ms,
                } => {
                    tracing::info!(
                        "MAPLOAD done box={} zooms={}-{} bytes={} fetched={} elapsed_ms={elapsed_ms} file={} evicted={}",
                        box_words(&want),
                        landed.zooms.0,
                        landed.zooms.1,
                        landed.bytes,
                        landed.fetched,
                        landed.name,
                        landed.evicted.len()
                    );
                    let regions = self.maps.headers();
                    let zooms = leviculum_map::choice::view_zooms(regions.iter());
                    let setup = MapSetup {
                        zooms: (*zooms.start(), *zooms.end()),
                        regions,
                        ..self.model.map_setup.clone()
                    };
                    self.apply(UiEvent::MapSetup(setup));
                    self.apply(UiEvent::MapLoad(MapLoad::Done(want)));
                }
                Step::Failed {
                    want,
                    detail,
                    toast,
                    elapsed_ms,
                } => {
                    tracing::info!(
                        "MAPLOAD failed box={} zooms={}-{} detail={detail:?} elapsed_ms={elapsed_ms}",
                        box_words(&want),
                        want.zooms.0,
                        want.zooms.1
                    );
                    self.apply(UiEvent::MapLoad(MapLoad::Failed { want, toast }));
                }
                Step::Skipped { want, why } => {
                    tracing::info!("MAPLOAD skipped box={} why={why}", box_words(&want));
                }
            }
        }
    }

    /// What the watching link said: a wake past the cursor pulls, and while
    /// the link is down a pull runs every minute, the terminal being in
    /// front whenever it runs.
    fn watched(&mut self) {
        let Some((_, events)) = &mut self.watch else {
            return;
        };
        let mut wake = false;
        while let Ok(event) = events.try_recv() {
            match event {
                WatchEvent::Watching(up) => self.watching = up,
                WatchEvent::Wake(newest) => {
                    let cursor = loquax::home::client::load_cursor(&self.context.home);
                    wake |= watch::wants_pull(newest, cursor);
                }
            }
        }
        if self.fallback.due(self.elapsed_ms(), self.watching, true) {
            tracing::info!("SYNC the watching link is down; pulling");
            wake = true;
        }
        if wake {
            self.apply(UiEvent::HomeWake);
        }
    }

    /// What a job starting now runs with: the nodes excluded at this moment.
    fn job_context(&self) -> RunContext {
        let mut context = self.context.clone();
        context.excluded = self.unanswered.excluded(self.elapsed_ms());
        context
    }

    /// The fetch ended: said in `tui.log`, judged for the mailbox's streak,
    /// and folded.
    fn fetch_done(&mut self, end: FetchEnd) {
        let reason = self.fetch_task.take().map_or("key", |(_, reason)| reason);
        // What the pull brought was written as the screen folded it, in the
        // poll that handed the runner its fence; only now may the cursor
        // pass it.
        runner::settle_pull(&self.context.home, end.cursor);
        if end.imported > 0 {
            self.show_imported(end.imported);
        }
        match &end.result {
            Ok(new) => tracing::info!(
                "SYNC done reason={reason} result=ok new={new} elapsed_ms={}",
                end.elapsed_ms
            ),
            Err(error) => {
                let text = error.to_string();
                let detail = text.lines().next().unwrap_or_default();
                tracing::info!(
                    "SYNC done reason={reason} result=failed detail={detail:?} elapsed_ms={}",
                    end.elapsed_ms
                );
            }
        }
        self.judge(&end);
        self.apply(UiEvent::FetchDone(end.result.map_err(|e| e.to_string())));
    }

    /// The history a pull or the palette's `import` wrote into the store,
    /// shown now (1159, 1162): the model's pending writes go first, so what
    /// it reads back is whole.
    fn show_imported(&mut self, imported: usize) {
        self.persist();
        match runner::show_imported(&mut self.model, &self.store) {
            Ok(()) => tracing::info!("SYNC imported rows={imported} shown"),
            Err(error) => tracing::warn!("SYNC imported rows={imported} not shown: {error}"),
        }
    }

    /// Count the fetch toward its node's streak. A node that did not answer
    /// [`UNANSWERED_LIMIT`] syncs in a row is excluded, and the next
    /// selection replaces it; one the user chose is only reported
    /// (`docs/src/concepts/loquax-mailbox.md`, section 1, "Which node").
    fn judge(&mut self, end: &FetchEnd) {
        let Some(node) = end.node else {
            return;
        };
        let error = match &end.result {
            Ok(_) => {
                self.unanswered.answered(node);
                return;
            }
            Err(error) => error,
        };
        if !self.unanswered.failed(node, error.before_link()) {
            return;
        }
        let by_user = self.context.preferred_pn.is_some()
            || mailbox_state::load(&self.context.home)
                .is_some_and(|stored| stored.node == node && stored.chosen_by_user);
        if by_user {
            tracing::info!(
                "MAILBOX unanswered node={} syncs={UNANSWERED_LIMIT} kept=user",
                loquax::address::to_hex(&node)
            );
            self.apply(UiEvent::Notice(format!(
                "mailbox {} did not answer {UNANSWERED_LIMIT} syncs in a row; you chose it, \
                 so it stays",
                short(&node)
            )));
            return;
        }
        tracing::info!(
            "MAILBOX unanswered node={} syncs={UNANSWERED_LIMIT} excluded_ms={UNANSWERED_EXCLUSION_MS}",
            loquax::address::to_hex(&node)
        );
        self.unanswered.exclude(node, self.elapsed_ms());
        self.replacing = Some(node);
    }

    /// A runner kept a selection: when it adopted a node in place of one
    /// that stopped answering, say both.
    fn replaced(&mut self, stored: &Stored) {
        let Some(old) = self.replacing else {
            return;
        };
        if stored.chosen_by_user || stored.node == old {
            return;
        }
        self.replacing = None;
        let line = format!(
            "propagation node {} adopted, {} did not answer",
            short(&stored.node),
            short(&old)
        );
        tracing::info!("{line}");
        self.apply(UiEvent::Notice(line));
    }

    /// Before leaving: hand the running send and fetch what the engine has
    /// said and give them [`SETTLE_MS`] to read it. Without this a send whose proof the screen
    /// had already shown was cut off before its runner logged `sending` and
    /// `proof`: a script that quits on `until delivery delivered` leaves in
    /// the same pump that delivered the proof (leviculum 1014).
    pub(crate) async fn settle(&mut self) {
        // The marks the reader made before leaving reach the home node, or
        // run out of a send's budget.
        self.hand_marks();
        let budget = tokio::time::Instant::now() + self.context.timeout;
        for task in std::mem::take(&mut self.mark_tasks) {
            if tokio::time::timeout_at(budget, task).await.is_err() {
                tracing::warn!("HOME mark still running at the end; given up");
            }
        }
        let mut tasks: Vec<tokio::task::JoinHandle<()>> =
            self.send_task.take().into_iter().collect();
        tasks.extend(self.fetch_task.take().map(|(task, _)| task));
        if tasks.is_empty() {
            return;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(SETTLE_MS);
        loop {
            if let Err(_gone) = self.fanout.pump(&mut self.attached.outbox) {
                break;
            }
            if tasks.iter().all(|task| task.is_finished()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(PUMP_MS)).await;
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        for task in tasks {
            task.abort();
        }
    }
}

/// A load's box as `tui.log` writes it: west, south, east, north in
/// degrees.
fn box_words(want: &MapWant) -> String {
    let d = |e7: i32| format!("{:.5}", f64::from(e7) / 1e7);
    let b = &want.bounds;
    format!("{},{},{},{}", d(b.west), d(b.south), d(b.east), d(b.north))
}

/// The header's mailbox state for a kept selection.
fn chosen(stored: &Stored) -> UiEvent {
    UiEvent::MailboxChosen {
        node: stored.node,
        by_user: stored.chosen_by_user,
    }
}

/// A key script's clock before its first `clock` step.
pub(crate) fn script_clock() -> WallTime {
    let (year, month, day, hour, minute) = SCRIPT_CLOCK;
    WallTime::from_civil(year, month, day, hour, minute, 0)
}

/// The wall clock with the host's UTC offset at this instant.
pub(crate) fn local_now() -> WallTime {
    let unix_secs = leviculum_std::local_time::unix_now_secs();
    WallTime {
        unix_secs,
        offset_secs: leviculum_std::local_time::utc_offset_secs(unix_secs),
    }
}

/// One compose-line send, exactly as `loquax send --via auto` runs it.
async fn send(
    mut tap: Tap,
    context: RunContext,
    to: [u8; 16],
    body: String,
    fields: Vec<Field>,
) -> UiEvent {
    if let Some(client) = &context.client {
        let settle = Duration::from_millis(SETTLE_MS);
        return runner::send_home(&mut tap, client, to, body, fields, context.timeout, settle)
            .await;
    }
    let mut options = SendOptions::new(context.instance.clone(), context.timeout);
    let mut effects = context.effects.clone();
    let (home, config_dir, instance, pn, excluded) = (
        context.home.clone(),
        context.config_dir.clone(),
        context.instance.clone(),
        context.preferred_pn,
        context.excluded.clone(),
    );
    let request = SendRequest {
        destination: to,
        title: Vec::new(),
        body: body.into_bytes(),
        fields,
        via: Via::Link,
    };
    let outcome = run_send_auto(
        &mut tap,
        request,
        context.preferred_pn,
        context.pn_source,
        &mut options,
        || async move { crate::mailbox_run(&home, &config_dir, &instance, pn, excluded).await },
        &mut effects,
        &Tokio::new(),
    )
    .await;
    match outcome {
        Ok(_) => UiEvent::SendDone(Ok(())),
        Err(AutoError::Direct(error @ SendError::Unproven { .. })) => {
            UiEvent::SendUnproven(error.to_string())
        }
        Err(AutoError::Direct(error)) => UiEvent::SendDone(Err(error.to_string())),
        Err(AutoError::Both { direct, fallback }) => {
            UiEvent::SendDone(Err(format!("direct: {direct}\npropagated: {fallback}")))
        }
    }
}

/// One mailbox drain, exactly as `loquax fetch` runs it.
async fn fetch(tap: Tap, context: RunContext) -> FetchEnd {
    let started = tokio::time::Instant::now();
    let mut watched = Watched { tap, node: None };
    let (result, cursor, imported) = match &context.client {
        Some(client) => {
            let end = runner::pull(&mut watched, client, &context.home, context.timeout).await;
            (
                end.result.map_err(FetchError::SyncFailed),
                end.cursor,
                end.imported,
            )
        }
        None => (drain(&mut watched, context).await, None, 0),
    };
    FetchEnd {
        result,
        node: watched.node,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        cursor,
        imported,
    }
}

async fn drain(outbox: &mut Watched, context: RunContext) -> Result<usize, FetchError> {
    let mut seen = SeenStore::load(&context.home)
        .map_err(|e| FetchError::SyncFailed(format!("seen store: {e}")))?;
    // A drain gets twice a send's budget, as on the command line.
    let mut options = FetchOptions::new(context.instance.clone(), context.timeout * 2);
    options.mailbox = crate::mailbox_run(
        &context.home,
        &context.config_dir,
        &context.instance,
        context.preferred_pn,
        context.excluded.clone(),
    )
    .await;
    let mut effects = context.effects.clone();
    run_fetch(
        outbox,
        context.preferred_pn,
        context.pn_source,
        &options,
        &mut effects,
        &mut seen,
        &Tokio::new(),
    )
    .await
    .map(|fetched| fetched.new)
}

/// Our contact code for `my code`, with its QR where the build draws one
/// (the terminal's; the window shows the code alone). `None` when the key
/// makes no address, which `my code` then says.
fn own_code(public_key: &[u8; loquax::lxma::KEY_BYTES]) -> Option<loquax::ui::OwnCode> {
    let code = loquax::lxma::encode(public_key).ok()?;
    #[cfg(feature = "qr")]
    let qr = loquax::home::qr::lines(&code).unwrap_or_default();
    #[cfg(not(feature = "qr"))]
    let qr = Vec::new();
    Some(loquax::ui::OwnCode { code, qr })
}

/// Everything before the first frame: the log, the identity, the history
/// and what it restores, the daemon, the contacts and the config, the
/// mailbox's schedule and the map's regions. The config comes back beside
/// the shell for what only a frontend reads (the theme). An `Err` is the
/// exit the run ends with, said already.
pub(crate) async fn start(
    options: &ShellOptions,
) -> Result<(Shell, loquax::config::Config), ExitCode> {
    let home = identity::home_dir().map_err(|error| crate::failure(error, "no-home"))?;
    // A full screen has no stderr to spare: the warnings a CLI run prints go
    // to a file beside the identity instead, with loquax's own `info` lines,
    // the `SEND` step of every leg, the `PATH` answers and the `RPC` calls,
    // so a send that waits can be read back afterwards (leviculum 1002).
    // `RUST_LOG` replaces the filter whole.
    leviculum_std::event_log::install_global_subscriber_to_file(
        LOG_FILTER,
        &home.join(options.log_file),
    );
    let (preferred_pn, pn_source) =
        crate::resolve_pn(options.pn.as_deref(), &home).map_err(crate::usage)?;
    // A client of a home node runs on its own device identity and shows
    // the home node's address as its own; its history is that address's.
    let client = loquax::home::client::load_client(&home)
        .map_err(|error| crate::failure(error, "no-home"))?;
    let (identity, address) = match client {
        Some(public_key) => {
            let device =
                identity::load_or_create(&home.join(loquax::home::client::DEVICE_IDENTITY_FILE))
                    .map_err(|error| crate::failure(error, "no-identity"))?;
            let address = loquax::home::lxmf_address(&public_key)
                .map_err(|error| crate::failure(error, "no-identity"))?;
            (device, address)
        }
        None => {
            let identity = identity::load_or_create(&home.join("identity"))
                .map_err(|error| crate::failure(error, "no-identity"))?;
            // The history, before the daemon: a damaged store stops the
            // start with the file named, and nothing is written over it.
            let address = loquax::engine::delivery_address(&identity)
                .map_err(|error| crate::failure(error, "no-identity"))?;
            (identity, address)
        }
    };
    // The messenger registers the delivery destination, as the home node
    // does: never both on one home. A client registers none.
    let delivery_lock = match client {
        Some(_) => None,
        None => Some(
            loquax::home::lock::DeliveryLock::acquire(&home, "the messenger")
                .map_err(|error| crate::failure(error, "locked"))?,
        ),
    };
    let store =
        Store::open_in(&home, address).map_err(|error| crate::failure(error, "no-store"))?;
    let history = store
        .restore()
        .and_then(|restored| {
            Ok((
                restored,
                store.names()?,
                store.last_key()?,
                store.positions()?,
                store.unverified()?,
            ))
        })
        .map_err(|error| crate::failure(error, "no-store"))?;
    let display_name = display_name::from_process(None).map_err(crate::usage)?;
    let config_dir = options
        .config
        .clone()
        .unwrap_or_else(leviculum_std::config::Config::default_config_dir);
    let instance = options
        .instance
        .clone()
        .unwrap_or_else(|| crate::instance_name(&config_dir));
    let storage_dir =
        crate::storage_dir(&home).map_err(|error| crate::failure(error, "no-daemon"))?;

    let (notices_tx, notices_rx) = unbounded_channel();
    let (legs_tx, legs_rx) = unbounded_channel();
    let (mailbox_tx, mailbox_rx) = unbounded_channel();
    let effects = ScreenEffects {
        notices: notices_tx,
        legs: legs_tx,
        mailbox: mailbox_tx,
        files: home.clone(),
    };
    // The code a friend adds us by: the home node's for a client, as its
    // address is (1279, `my code`).
    let own_key = match client {
        Some(public_key) => public_key,
        None => identity.public_key_bytes(),
    };
    let device = identity.clone();
    let role = match client {
        Some(_) => Role::Client { address },
        None => Role::Messenger,
    };
    let attached = attach_as(
        AttachConfig {
            instance: instance.clone(),
            config_dir: Some(config_dir.clone()),
            storage_dir,
            identity,
            display_name: display_name.name.into_bytes(),
            notices: effects.clone(),
        },
        role,
    )
    .await
    .map_err(|error| crate::no_daemon(&error))?;
    let client = client.map(|public_key| HomeClient {
        public_key,
        device,
        node: attached.node(),
    });

    let config = loquax::config::load(&home).map_err(|error| crate::usage(error.to_string()))?;
    let contacts =
        loquax::names::load_contacts(&home).map_err(|error| crate::usage(error.to_string()))?;
    let mut model = Model::new(&options.to);
    model.home_client = client.is_some();
    model.own_code = own_code(&own_key);
    model.names = loquax::names::Names::new(contacts);
    let (restored, names, last_key, positions, unverified) = history;
    // The messages an earlier run kept unverified: the engine checks each
    // once its sender's key is known, and asks for it until then.
    for (message_id, source, packed) in unverified {
        if attached
            .outbox
            .submit(Command::Verify {
                message_id,
                source,
                packed,
            })
            .is_err()
        {
            break;
        }
    }
    for (peer, name) in &names {
        model.names.remember(*peer, name);
    }
    // The time the restart's ledger lines carry: the script's own clock, or
    // the wall clock.
    let start = match options.scripted {
        true => script_clock(),
        false => local_now(),
    };
    update(&mut model, UiEvent::Clock(start));
    model.restore(restored);
    model.restore_positions(positions);
    model.activity = model.activity.max(last_key);
    model.ascii = options.ascii;
    model.mouse = !options.no_mouse;
    model.send_on_enter = config.send_on_enter.unwrap_or(true);
    model.reading.on = config.read_receipts.unwrap_or(true);
    model.photo_size = config.photo_size.unwrap_or_default();
    // The microphone (1276): the system's recorder, where it has one; the
    // window offers the microphone only then.
    let mic = crate::mic::Mic::of_process();
    model.bar.abilities.microphone = mic.available();
    // The mailbox's schedule: the flag, else the config file, else fifteen
    // minutes. A script runs one only when its command line names the flag,
    // as it ticks only when it says so.
    let bound = match (options.scripted, options.sync_interval) {
        (_, Some(bound)) => Some(bound),
        (false, None) => Some(
            config
                .sync_interval
                .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
        ),
        (true, None) => None,
    };
    model.sync = bound.map(loquax::sync::Schedule::new);
    if let Some(bound) = bound {
        tracing::info!(
            "SYNC bound={}",
            bound.map_or("off".to_string(), |ms| format!("{}s", ms / 1000))
        );
    }
    // The mailbox the header names before any run: `--pn` or the configured
    // node, else what the last run kept.
    let mailbox = match preferred_pn {
        Some(node) => Some(UiEvent::MailboxChosen {
            node,
            by_user: true,
        }),
        None => mailbox_state::load(&home).as_ref().map(chosen),
    };
    if let Some(event) = mailbox {
        update(&mut model, event);
    }
    // The map's regions: a file that does not open is named in the log
    // and left out. The painters read the directory again whenever the
    // autoload changes it.
    let maps = Maps::new(loquax::picture::maps_dir());
    for problem in maps.load().1 {
        tracing::warn!("{problem}");
    }
    model.map_autoload = config.map_autoload.unwrap_or(true);
    model.map_theme = config.map_theme;
    let source = Source::of(config.map_source.as_deref());
    tracing::info!(
        "MAPLOAD autoload={} source={}",
        if model.map_autoload { "on" } else { "off" },
        match &source {
            Source::Url(url) => url.clone(),
            Source::Daily(base) => format!("{base}<newest>.pmtiles"),
        }
    );
    let cache_bytes = config.map_cache_mb.unwrap_or(CACHE_DEFAULT_MB) << 20;
    let loader = Loader::new(maps.clone(), source, cache_bytes);
    let (done_tx, done_rx) = unbounded_channel();
    let watch = match (&client, options.scripted) {
        (Some(client), false) => {
            let (events_tx, events_rx) = unbounded_channel();
            let task = tokio::spawn(watch::run(
                watch::Watch {
                    node: client.node.clone(),
                    public_key: client.public_key,
                    device: client.device.clone(),
                    home: home.clone(),
                },
                events_tx,
            ));
            Some((task, events_rx))
        }
        _ => None,
    };
    let shell = Shell {
        model,
        _delivery_lock: delivery_lock,
        store,
        store_failed: false,
        attached,
        fanout: Fanout::default(),
        context: RunContext {
            home,
            config_dir,
            instance,
            preferred_pn,
            pn_source,
            timeout: Duration::from_secs(options.timeout),
            effects,
            excluded: Vec::new(),
            client,
        },
        sends: VecDeque::new(),
        fetch_owed: None,
        send_task: None,
        fetch_task: None,
        mark_tasks: Vec::new(),
        unanswered: Unanswered::default(),
        replacing: None,
        legs_seen: Vec::new(),
        done_tx,
        done_rx,
        notices_rx,
        legs_rx,
        mailbox_rx,
        afterwards: Vec::new(),
        gone: false,
        redraw: false,
        outputs: Vec::new(),
        maps,
        loader,
        origin: tokio::time::Instant::now(),
        watch,
        watching: false,
        fallback: Fallback::default(),
        mic,
    };
    Ok((shell, config))
}

/// After the last frame: the running send and fetch settle, the engine
/// detaches, and what the screen kept for afterwards is printed.
pub(crate) async fn finish(mut shell: Shell, outcome: Result<(), String>) -> ExitCode {
    shell.settle().await;
    let Shell {
        attached,
        afterwards,
        watch,
        ..
    } = shell;
    if let Some((task, _)) = watch {
        task.abort();
    }
    attached.stop().await;
    for line in afterwards {
        eprintln!("loquax: {line}");
    }
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => crate::failure(error, "failed"),
    }
}
