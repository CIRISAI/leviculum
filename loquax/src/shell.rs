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
use loquax::ui::viewload::{ViewLoads, ViewWants};
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

/// The log file's filter without `RUST_LOG`: Loquax's own lines, and the
/// `INSTANCE` line that says whether the daemon was found or started, and
/// its pid (1570; the man page promised it, the filter dropped it).
const LOG_FILTER: &str = "warn,loquax=info,leviculum_std::instance=info";

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
    /// `--no-start`: fail when no shared instance answers instead of
    /// starting lnsd as a child (#471 step 4).
    pub no_start: bool,
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
    /// The shared instance found or started (#471 step 4): a daemon this
    /// shell started goes when it is dropped, after the engine detached.
    daemon: Option<leviculum_std::instance::Instance>,
    fanout: Fanout,
    /// The engine's outbox is lent to the terminal's reader thread
    /// ([`Shell::lend_engine`], 1492): the reads come in through
    /// [`Shell::fold_read`], and [`Shell::poll`] reads none itself.
    lent: bool,
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
    /// The terminal's map pane's scheduler (1527, [`ViewLoads`]): every
    /// frame of the open map is shown to it, and the screen at its zoom
    /// loads first. `None` in the window, whose map keeps its own view and
    /// sends the scheduler's decision ([`Shell::map_schedule`]).
    pub(crate) map_drive: Option<ViewLoads>,
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
    /// The Browse tab's fetch and what the browser asks of the desktop
    /// (1369).
    pub(crate) browsing: crate::browsing::Browsing,
    /// The Rooms' hub link (1379), where the config names a hub.
    rooming: loquax::rooms_link::Rooming,
    /// On a client of a home node: the running `/pages` ask (1579), one at
    /// a time.
    pages_task: Option<tokio::task::JoinHandle<()>>,
    /// On a client of a home node: the running `/about` ask (1642), and
    /// when the last began on the shell's clock.
    about_task: Option<tokio::task::JoinHandle<()>>,
    about_at: Option<u64>,
    /// How this device reaches its mailbox (1642), as the mailbox page
    /// says it: the watching link up or down since when, the last contact.
    reach: loquax::ui::mailbox_page::Reach,
}

/// How long the mailbox's word on itself stays fresh on a computer (1642):
/// asked after a pull at most this often.
const ABOUT_EVERY_MS: u64 = 10 * 60 * 1000;

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
        self.run(effects);
    }

    /// Fold one of lnomad's own events into the hosted browser (1369): a
    /// picture's, which no event of Loquax's carries.
    fn apply_app(&mut self, event: lnomad::AppEvent) {
        let effects = loquax::ui::browse::app(&mut self.model, event);
        // Not through `update`: the loops are told the tab moved (1491).
        self.model.changed_outside();
        self.run(effects);
    }

    /// The browser's events the desktop has for it: the fetch's, and what
    /// its effects answered at once.
    fn fold_browsed(&mut self, browsed: Vec<crate::browsing::Folded>) {
        for folded in browsed {
            match folded {
                crate::browsing::Folded::Ui(event) => self.apply(event),
                crate::browsing::Folded::App(event) => self.apply_app(event),
            }
        }
    }

    /// Write what a fold changed to the store, and run what came back.
    fn run(&mut self, effects: Vec<UiEffect>) {
        self.persist();
        let mut answers = Vec::new();
        let mut browsed = Vec::new();
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
                // The propagation node asked what became of our uploads
                // (1409), on a link of its own; the answer comes back on
                // the pump.
                UiEffect::AskPn { node, ids } => {
                    let reticulum = self.attached.node();
                    let done = self.done_tx.clone();
                    let budget = self.context.timeout;
                    self.mark_tasks.push(tokio::spawn(async move {
                        let answer =
                            loquax::home::client::pn_answer(&reticulum, node, &ids, budget).await;
                        let _ = done.send(Finished::Event(UiEvent::PnAnswered { node, answer }));
                    }));
                }
                UiEffect::PnLine(line) => tracing::info!("{line}"),
                // Explore opened (1644): the mailbox's list asked again.
                UiEffect::AskPages => self.ask_pages(),
                UiEffect::Quit => {}
                UiEffect::Redraw => self.redraw = true,
                UiEffect::Notice(line) => self.afterwards.push(line),
                UiEffect::Copy(text) => self.outputs.push(Output::Copy(text)),
                // The Browse tab (1369): the fetch, the clipboard, the
                // downloads and the tab's files; what the browser is told
                // at once is folded after this fold.
                UiEffect::Browse(effect) => {
                    browsed.extend(self.browsing.effect(
                        &mut self.model,
                        effect,
                        &mut self.outputs,
                    ));
                    self.model.changed_outside();
                }
                // The Rooms (1379): the hub link does it; what the hub says
                // comes back on the pump.
                UiEffect::Rooms(ask) => self.rooming.ask(ask),
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
                // The palette's `delete all maps` (1371): the files go, as
                // the window's settings button deletes them, and the model
                // learns the regions left.
                // A path typed into the terminal's palette (1371): read as
                // the window's chooser hands one over.
                UiEffect::Attach { peer, path, photo } => {
                    self.attach(peer, &home_path(&path), photo);
                }
                UiEffect::ScanCode { path } => {
                    let path = home_path(&path);
                    let read = std::fs::read(&path)
                        .map_err(|e| format!("{}: {e}", path.display()))
                        .and_then(|bytes| loquax::scan::image_codes(&bytes));
                    answers.push(match read.map(|codes| codes.into_iter().next()) {
                        Ok(Some(code)) => UiEvent::CodeRead(code),
                        Ok(None) => UiEvent::Notice(
                            loquax::ui::strings::StrId::CodeNotInPicture
                                .text(self.model.lang)
                                .to_string(),
                        ),
                        Err(line) => UiEvent::Notice(line),
                    });
                }
                UiEffect::DeleteAllMaps => {
                    let line = match self.maps.delete_all() {
                        Ok(gone) => format!("deleted {gone} maps"),
                        Err(why) => why,
                    };
                    self.regions_changed();
                    answers.push(UiEvent::Notice(line));
                }
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
                        loquax::ui::groups::NotMade::Failed(
                            "groups live at a mailbox, and this Loquax has none".to_string(),
                        ),
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
                UiEffect::DeleteGroup { group } => match self.context.client.clone() {
                    Some(client) => {
                        let done = self.done_tx.clone();
                        let budget = self.context.timeout;
                        self.mark_tasks.push(tokio::spawn(async move {
                            let event = runner::delete_group(client, group, budget).await;
                            let _ = done.send(Finished::Event(event));
                        }));
                    }
                    None => answers.push(UiEvent::GroupDeleted(Err(
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
                UiEffect::RememberHash { key, hash } => {
                    if let Err(error) = loquax::config::save_hash(&self.context.home, key, hash) {
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
        self.fold_browsed(browsed);
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
            Ok(contacts) => {
                self.model.names.contacts = contacts;
                self.model.changed_outside();
            }
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
        self.hand_bookmarks();
        // What this device heard (1632): the next start lists it again.
        let heard = loquax::ui::explore::take_heard(&mut self.model);
        if heard != loquax::ui::explore::HeardOwed::default() {
            if let Err(error) = loquax::heard::keep(&mut self.store, &self.context.home, heard) {
                tracing::warn!("HEARD not kept: {error}");
            }
        }
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
    /// task of their own, as marks are (1139); what the home took comes
    /// back as an event, the rest stays owed (1635).
    fn hand_petnames(&mut self) {
        // Taken only with a client to hand them to: a take waits for its
        // answer before the next (1635).
        let Some(client) = self.context.client.clone() else {
            return;
        };
        let petnames = self.model.take_petnames();
        if petnames.is_empty() {
            return;
        }
        self.mark_tasks.retain(|task| !task.is_finished());
        let budget = self.context.timeout;
        let done = self.done_tx.clone();
        self.mark_tasks.push(tokio::spawn(async move {
            let event = runner::hand_petnames(client, petnames, budget).await;
            let _ = done.send(Finished::Event(event));
        }));
    }

    /// The peer states the reader set (1146), handed to the home node as
    /// petnames are, and owed until it takes them (1640).
    fn hand_peer_states(&mut self) {
        // Taken only with a client to hand them to: a take waits for its
        // answer before the next (1640).
        let Some(client) = self.context.client.clone() else {
            return;
        };
        let states = self.model.take_peer_states();
        if states.is_empty() {
            return;
        }
        self.mark_tasks.retain(|task| !task.is_finished());
        let budget = self.context.timeout;
        let done = self.done_tx.clone();
        self.mark_tasks.push(tokio::spawn(async move {
            let event = runner::hand_peer_states(client, states, budget).await;
            let _ = done.send(Finished::Event(event));
        }));
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

    /// Fold the engine's events the fan-out took into the screen and the
    /// store.
    fn fold(&mut self, events: Vec<OutboxEvent>) {
        for event in events {
            let named = match &event {
                OutboxEvent::Announced {
                    destination,
                    name: Some(_),
                    ..
                } => Some(*destination),
                _ => None,
            };
            if runner::already_held(&self.store, &event) {
                continue;
            }
            runner::keep_signature(&mut self.store, &event);
            self.apply(UiEvent::Engine(event));
            if let Some(destination) = named {
                runner::keep_name(&mut self.store, &self.model.names, destination);
            }
        }
    }

    /// What arrived since the last call, without waiting: a slice of it
    /// (1464, [`loquax::fanout::SLICE`]), the rest at the next pump. Each
    /// event folded is a store commit, and a first pull of a big mailbox
    /// queues thousands: drained in one call, as before 1464, the shell
    /// handled none of the window's or the terminal's input and sent them
    /// no new state until all of it was folded, 12.8 s on the host for
    /// 4000 entries, the phone's measure (1446).
    pub(crate) fn poll(&mut self) {
        let mut slicing = loquax::fanout::Slicing::new(loquax::fanout::SLICE);
        while !self.lent {
            match slicing.next(&mut self.fanout, &mut self.attached.outbox, false) {
                Ok(Some(events)) => self.fold(events),
                Ok(None) => break,
                Err(gone) => {
                    self.engine_gone(gone);
                    break;
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
        self.drive_map();
        self.apply(UiEvent::Elapsed(self.elapsed_ms()));
        self.listen();
        let browsed = self.browsing.told(&self.model);
        self.fold_browsed(browsed);
        for told in self.rooming.told() {
            self.apply(told);
        }
    }

    /// The engine stopped answering: said once, on screen and after it.
    fn engine_gone(&mut self, gone: OutboxGone) {
        if !self.gone {
            self.gone = true;
            self.apply(UiEvent::Notice(gone.to_string()));
            self.afterwards.push(gone.to_string());
        }
    }

    /// Lend the engine's outbox to a reader on a thread of its own (1492):
    /// the terminal reads the engine there, so an engine step that stalls
    /// never holds a key back. `None` when it is lent already.
    #[cfg(feature = "tui")]
    pub(crate) fn lend_engine(
        &mut self,
    ) -> Option<loquax::fanout::Reader<loquax::engine::InProcessOutbox>> {
        let outbox = std::mem::replace(
            &mut self.attached.outbox,
            loquax::engine::InProcessOutbox::severed(),
        );
        match self.fanout.lend(outbox) {
            Ok(reader) => {
                self.lent = true;
                Some(reader)
            }
            Err(outbox) => {
                self.attached.outbox = outbox;
                None
            }
        }
    }

    /// The engine's outbox back from the reader: [`Shell::poll`] and
    /// [`Shell::settle`] read it again.
    #[cfg(feature = "tui")]
    pub(crate) fn take_engine_back(
        &mut self,
        reader: loquax::fanout::Reader<loquax::engine::InProcessOutbox>,
    ) {
        self.attached.outbox = self.fanout.take_back(reader);
        self.lent = false;
    }

    /// What the reader read, handed to the taps and folded as
    /// [`Shell::poll`] folds what it reads itself.
    #[cfg(feature = "tui")]
    pub(crate) fn fold_read(&mut self, read: Result<Vec<OutboxEvent>, OutboxGone>) {
        match read {
            Ok(events) => {
                self.fanout.deliver(&events);
                self.fold(events);
            }
            Err(gone) => self.engine_gone(gone),
        }
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

    /// A file the desktop's chooser picked for `peer`'s chat (1341), sent
    /// as the phone sends what its picker hands over: a picture scaled and
    /// stripped at the reader's photo size, any other file whole up to
    /// what a receiver takes; what will not go is said in the reader's
    /// words, as the phone's status line says it.
    pub(crate) fn attach(&mut self, peer: [u8; 16], path: &std::path::Path, photo: bool) {
        let lang = self.model.lang;
        let event = std::fs::metadata(path)
            .map_err(|e| format!("{}: {e}", path.display()))
            .and_then(|meta| {
                let whole = usize::try_from(meta.len()).unwrap_or(usize::MAX);
                match loquax::ui::files::refusal(whole).filter(|_| !photo) {
                    Some(why) => Err(why.resolve(lang)),
                    None => std::fs::read(path).map_err(|e| format!("{}: {e}", path.display())),
                }
            })
            .and_then(|data| match photo {
                true => loquax::photo::prepare(&data, self.model.photo_size)
                    .map(|prepared| UiEvent::SendPhoto {
                        peer,
                        format: prepared.format.to_string(),
                        data: prepared.data,
                    })
                    .map_err(|e| format!("{}: {e}", path.display())),
                false => Ok(UiEvent::SendFile {
                    peer,
                    name: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    data,
                }),
            });
        match event {
            Ok(event) => self.apply(event),
            Err(line) => self.apply(UiEvent::Notice(line)),
        }
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
        // The browser's spinner and its toast's age, while it is shown.
        if self.model.browse.showing() {
            self.apply(UiEvent::Browse(loquax::ui::browse::BrowseEvent::Tick));
        }
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
                let told = self.done_tx.clone();
                self.send_task = Some(tokio::spawn(async move {
                    // How far a client's upload went (1662), folded as
                    // it comes, before the send's end.
                    let mut progress = move |event: UiEvent| {
                        let _ = told.send(Finished::Event(event));
                    };
                    let event = send(tap, context, to, body, fields, &mut progress).await;
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

    /// The scheduler's decision for the screen (1527, 1292): the window's
    /// map sends it, the terminal's pane makes it in [`Shell::drive_map`].
    pub(crate) fn map_schedule(&mut self, wants: ViewWants) {
        if !wants.here.is_empty() {
            tracing::info!(
                "MAPLOAD view share={:.2} here={} ahead={}",
                wants.share,
                wants.here.len(),
                wants.ahead.len()
            );
        }
        let steps = self
            .loader
            .schedule(wants.here, wants.ahead, self.elapsed_ms());
        self.map_steps(steps);
    }

    /// The terminal's open map shown to the scheduler (1527): every pump
    /// decides for the view the pane draws, a decision every
    /// [`loquax::ui::viewload::DECIDE_MS`] while a drag moves it.
    fn drive_map(&mut self) {
        let Some(loads) = self.map_drive.as_mut() else {
            return;
        };
        let Some(state) = self.model.map.as_ref().filter(|_| self.model.map_autoload) else {
            return;
        };
        let view = loquax::ui::map::viewport(state, self.model.map_area(), &self.model.map_setup);
        let moving = self.model.map_dragging();
        let now = u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(wants) = loads.decide(&view, &self.model.map_setup.regions, now, moving, None) {
            self.map_schedule(wants);
        }
    }

    /// The regions on disk, told to the model's map setup.
    fn regions_changed(&mut self) {
        let regions = self.maps.headers();
        let zooms = leviculum_map::choice::view_zooms(regions.iter());
        let setup = MapSetup {
            zooms: (*zooms.start(), *zooms.end()),
            regions,
            ..self.model.map_setup.clone()
        };
        self.apply(UiEvent::MapSetup(setup));
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
                    self.regions_changed();
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
        // Whether a load runs or waits for the screen (1527): the
        // window's chip and the terminal's title say so.
        let here = self.loader.loading_here();
        if here != self.model.map_here {
            self.apply(UiEvent::MapLoad(MapLoad::Here(here)));
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
        let mut told_reach = None;
        while let Ok(event) = events.try_recv() {
            match event {
                WatchEvent::Watching(up) => {
                    self.watching = up;
                    told_reach = Some(up);
                }
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
        if let Some(up) = told_reach {
            self.reached(Some(up));
        }
        if wake {
            self.apply(UiEvent::HomeWake);
        }
    }

    /// The mailbox page's connection (1642): `up` the watching link's
    /// word, `None` for a pull that came back (a contact, the link as it
    /// was). Told to the model when it moved.
    fn reached(&mut self, up: Option<bool>) {
        let now = leviculum_std::local_time::unix_now_secs();
        let mut reach = self.reach.clone();
        if let Some(up) = up {
            if reach.up != Some(up) {
                reach.up = Some(up);
                reach.since = Some(now);
                // Explore says how old the mailbox's list is (1644).
                self.apply(UiEvent::Explore(
                    loquax::ui::explore::ExploreEvent::Mailbox(up),
                ));
            }
        }
        if up != Some(false) {
            reach.last_contact = Some(now);
        }
        if reach != self.reach {
            self.reach = reach.clone();
            self.apply(UiEvent::MailboxReach(Box::new(reach)));
        }
    }

    /// On a client of a home node, after a pull: the mailbox's word on
    /// itself (1642), at most every [`ABOUT_EVERY_MS`], one ask at a time.
    fn ask_about(&mut self) {
        let Some(client) = self.context.client.clone() else {
            return;
        };
        let now = self.elapsed_ms();
        if self.about_task.as_ref().is_some_and(|t| !t.is_finished())
            || self
                .about_at
                .is_some_and(|at| now.saturating_sub(at) < ABOUT_EVERY_MS)
        {
            return;
        }
        self.about_at = Some(now);
        let budget = self.context.timeout;
        let done = self.done_tx.clone();
        self.about_task = Some(tokio::spawn(async move {
            if let Some(event) = runner::home_about(client, budget).await {
                let _ = done.send(Finished::Event(event));
            }
        }));
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
        // Once a pull settled, what arrives is live mail (1429).
        if end.cursor.is_some() {
            self.model.history = false;
            self.model.changed_outside();
        }
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
        let pulled = end.result.is_ok();
        self.judge(&end);
        self.apply(UiEvent::FetchDone(end.result.map_err(|e| e.to_string())));
        if pulled {
            self.ask_pages();
            if self.context.client.is_some() {
                self.reached(None);
                self.ask_about();
            }
        }
    }

    /// On a client of a home node, after a pull (1579): the pages the home
    /// heard and the bookmarks of the identity after the stamp this device
    /// holds, with the bookmark changes still pending handed first. One
    /// ask at a time.
    fn ask_pages(&mut self) {
        let Some(client) = self.context.client.clone() else {
            return;
        };
        if self
            .pages_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let pending = self.model.browse.shared.pending();
        let stamp = self.model.browse.shared.stamp;
        let budget = self.context.timeout;
        let done = self.done_tx.clone();
        tracing::info!("HOME pages asked since={stamp}");
        self.pages_task = Some(tokio::spawn(async move {
            if let Some(event) = runner::home_pages(client, pending, stamp, budget).await {
                let _ = done.send(Finished::Event(event));
            }
        }));
        loquax::ui::explore::pages_asked(&mut self.model);
    }

    /// The bookmark changes the reader made (1579), handed to the home on a
    /// task of their own, as petnames are; a device without a home keeps
    /// them to itself.
    fn hand_bookmarks(&mut self) {
        let rows = std::mem::take(&mut self.model.browse.outgoing);
        let Some(client) = self.context.client.clone() else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        self.mark_tasks.retain(|task| !task.is_finished());
        let budget = self.context.timeout;
        self.mark_tasks
            .push(tokio::spawn(runner::hand_bookmarks(client, rows, budget)));
    }

    /// The history a pull or the palette's `import` wrote into the store,
    /// shown now (1159, 1162): the model's pending writes go first, so what
    /// it reads back is whole.
    fn show_imported(&mut self, imported: usize) {
        self.persist();
        let shown = runner::show_imported(&mut self.model, &self.store);
        self.model.changed_outside();
        match shown {
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
        // run out of a send's budget, and so do the petnames and peer
        // states (1614, owed until the home takes them since 1640).
        self.hand_marks();
        self.hand_petnames();
        self.hand_peer_states();
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
    progress: &mut (dyn FnMut(UiEvent) + Send),
) -> UiEvent {
    if let Some(client) = &context.client {
        let settle = Duration::from_millis(SETTLE_MS);
        return runner::send_home(
            &mut tap,
            client,
            to,
            body,
            fields,
            context.timeout,
            settle,
            progress,
        )
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

/// Our contact code for the profile (1443), with its QR where the build
/// encodes one: verbatim in byte mode, the code the phone's profile and a
/// friend's scanner read. `None` when the key makes no address.
fn own_code(public_key: &[u8; loquax::lxma::KEY_BYTES]) -> Option<loquax::ui::OwnCode> {
    let code = loquax::lxma::encode(public_key).ok()?;
    #[cfg(feature = "qr")]
    let qr = loquax::home::qr::verbatim_modules(&code).ok();
    #[cfg(not(feature = "qr"))]
    let qr = None;
    Some(loquax::ui::OwnCode::of(code, qr))
}

/// What the profile shows of the reader that the model does not know
/// (1443): the name the announce carries, the mailbox, this device. A
/// client's mailbox is another device's, reached over this computer's
/// node; a messenger without one has none.
fn profile_facts(name: &str, client: bool) -> loquax::ui::profile::ProfileFacts {
    use loquax::ui::profile::{Device, Mailbox, ProfileFacts};
    ProfileFacts {
        name: name.to_string(),
        mailbox: match client {
            true => Mailbox::Away {
                name: None,
                rung: None,
                word: None,
            },
            false => Mailbox::Unknown,
        },
        devices: vec![Device {
            name: String::new(),
            this: true,
        }],
        ..Default::default()
    }
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
    // The start reads the summaries, not the bodies (1490, schema 17):
    // each conversation's page loads when it opens, through the same
    // store reads the scrollback uses.
    let history = store
        .start()
        .and_then(|restored| {
            Ok((
                restored,
                store.names()?,
                store.last_key()?,
                store.positions()?,
                store.unverified()?,
                store.fixes_between(None, i64::MIN, i64::MAX)?,
                store.pn_asks()?,
                store.owed_petnames()?,
                store.owed_peer_states()?,
            ))
        })
        .map_err(|error| crate::failure(error, "no-store"))?;
    let display_name = display_name::from_process(None).map_err(crate::usage)?;
    // The terminal and the window attach to a shared instance; where none
    // answers they start lnsd as their child (#471 step 4), which goes when
    // they do. Quiet: the screen is stderr, the line goes to the log file.
    // Every running daemon the lookup names is tried before one is started
    // (1598).
    let lookup = crate::daemon_lookup(options.config.clone(), options.instance.clone());
    let mut daemon = None;
    let mut answered = lookup.start.clone();
    for look in &lookup.looks {
        let mut request = leviculum_std::instance::Request::new(&look.dir, &look.instance);
        request.no_start = true;
        request.report = leviculum_std::instance::Report::Quiet;
        request.source = Some(look.source.as_str());
        if let Ok(Some(found)) = leviculum_std::instance::for_request(&request) {
            daemon = Some(found);
            answered = look.clone();
            break;
        }
    }
    if daemon.is_none() {
        let start = &lookup.start;
        let mut request = leviculum_std::instance::Request::new(&start.dir, &start.instance);
        request.no_start = options.no_start;
        request.report = leviculum_std::instance::Report::Quiet;
        request.source = Some(start.source.as_str());
        daemon = leviculum_std::instance::for_request(&request)
            .map_err(|error| crate::no_start(error, &start.instance))?;
    }
    let crate::Look {
        dir: config_dir,
        instance,
        ..
    } = answered;
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
    // The identity a NomadNet node is shown when the reader identifies to
    // it from the Browse tab: the one this node runs on, as on the phone.
    let browse_identity = identity.clone();
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
            display_name: display_name.name.clone().into_bytes(),
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
    // A client that never settled a pull is about to be handed the home's
    // history (1429): none of it is read by arriving.
    model.history = model.home_client && loquax::home::client::history_owed(&home);
    model.own_code = own_code(&own_key);
    model.profile = profile_facts(&display_name.name, model.home_client);
    // This device in the mailbox's list (1642): the identity it joined with.
    model.profile.device = client.as_ref().map(|c| *c.device.hash());
    model.profile.version = loquax::VERSION.to_string();
    // Only the contacts: the names already know the peers `Model::new`
    // opened conversations with (1452, two nameless ones collide).
    model.names.contacts = contacts;
    let (
        restored,
        names,
        last_key,
        positions,
        unverified,
        track_fixes,
        pn_asks,
        owed_petnames,
        owed_peer_states,
    ) = history;
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
    // The open conversation's page, read here before the first frame
    // (1490): the start read handed summaries alone, and the screen
    // opens on a filled pane as it always did. Every later selection
    // loads through the fold's own ask.
    if let Some((peer, true)) = model
        .conversations
        .get(model.selected)
        .map(|c| (c.peer, c.messages.is_empty() && c.earlier > 0))
    {
        match store.earlier(peer, u64::MAX, None) {
            Ok((messages, earlier)) => {
                update(
                    &mut model,
                    UiEvent::Earlier {
                        peer,
                        messages,
                        earlier,
                    },
                );
            }
            Err(error) => return Err(crate::failure(error, "no-store")),
        }
    }
    // What the propagation node is still to be asked about (1409).
    model.pn_asker = loquax::pn_status::Asker::restore(pn_asks);
    // The petnames the home node had not taken when loquax ended (1635):
    // handed again, and the reader's word over what the first pull brings.
    model.restore_owed_petnames(owed_petnames);
    // The peer states likewise (1640): a pin loosened before the end
    // stays loosened.
    model.restore_owed_peer_states(owed_peer_states);
    model.restore_positions(positions);
    model.restore_tracks(track_fixes);
    model.activity = model.activity.max(last_key);
    model.ascii = options.ascii;
    model.mouse = !options.no_mouse;
    model.send_on_enter = config.send_on_enter.unwrap_or(true);
    model.reading.on = config.read_receipts.unwrap_or(true);
    model.photo_size = config.photo_size.unwrap_or_default();
    // The browser (1369), opened from Explore: on unless the config's
    // `browse_tab` says off (1618: Explore is the browser's way in, and a
    // tab that lists pages it cannot open is broken). On, its fetch runs
    // over the node attached above; off, the node is lent to nobody.
    let browse_tab = config.browse_on();
    crate::browsing::set_up(&mut model, &home, browse_tab);
    let browsing = match browse_tab {
        true => crate::browsing::Browsing::start(&attached, browse_identity.clone(), &home),
        false => crate::browsing::Browsing::off(),
    };
    // The Rooms (1379): a tab where the config names a hub, and a hub
    // link over the node attached above, identified with the reader's
    // identity and shown by the reader's name. The link runs with no hub
    // too, idle until asked (1577): the settings choose a hub while the
    // app runs, and the tab appears at once.
    model.rooms.hub = config.rooms_hub;
    model.rooms.enabled = config.rooms_hub.is_some();
    model.rooms.told = config.rooms_told;
    // The Explore tab's order (1578), per device.
    model.explore.az = config.explore_az.unwrap_or(false);
    // What this device heard before (1632): its pages, hubs and the
    // suggestion, each with its old last seen.
    match loquax::heard::load(&store) {
        Ok((pages, hubs)) => {
            loquax::ui::explore::restore_heard(&mut model, pages, hubs, config.rooms_suggested)
        }
        Err(error) => tracing::warn!("HEARD not read: {error}"),
    }
    let nick = Some(display_name.name.clone()).filter(|n| !n.is_empty());
    let rooming = loquax::rooms_link::Rooming::start(&attached, browse_identity, nick);
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
        daemon,
        fanout: Fanout::default(),
        lent: false,
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
        map_drive: Some(ViewLoads::new()),
        origin: tokio::time::Instant::now(),
        watch,
        watching: false,
        fallback: Fallback::default(),
        mic,
        browsing,
        rooming,
        pages_task: None,
        about_task: None,
        about_at: None,
        reach: loquax::ui::mailbox_page::Reach::default(),
    };
    Ok((shell, config))
}

/// After the last frame: the running send and fetch settle, the engine
/// detaches, and what the screen kept for afterwards is printed.
pub(crate) async fn finish(mut shell: Shell, outcome: Result<(), String>) -> ExitCode {
    shell.settle().await;
    let Shell {
        attached,
        daemon,
        afterwards,
        watch,
        ..
    } = shell;
    if let Some((task, _)) = watch {
        task.abort();
    }
    attached.stop().await;
    drop(daemon);
    for line in afterwards {
        eprintln!("loquax: {line}");
    }
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => crate::failure(error, "failed"),
    }
}

/// A path as typed (1371): a leading `~` is the home directory.
fn home_path(typed: &str) -> std::path::PathBuf {
    match (typed.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::PathBuf::from(home).join(rest),
        _ => std::path::PathBuf::from(typed),
    }
}
