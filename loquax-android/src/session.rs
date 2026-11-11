//! The app's session: a leaf node with one TCP interface to the home node
//! (`loquax::engine::attach_leaf`), the client engine behind the Outbox
//! seam, the store, and loquax's screen model, which decides what a
//! message, a send, a mark and a pull do to the conversations exactly as it
//! does in the terminal. The model's effects run here on a tokio runtime the
//! session owns, through the client runners the terminal's shell runs too
//! (`loquax::home::runner`): sends through `/send` one at a time, pulls,
//! marks.
//!
//! Java never touches the engine. It calls [`Session::request`] and
//! [`Session::query`] from any thread, which take the session's lock
//! for as long as a fold or a read takes, and waits for
//! [`Session::next_event`] on a thread of its own.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use leviculum_lxmf::Field;
use leviculum_ui::commands::Command as PaletteCommand;
use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::engine::{attach_leaf, Attached, LeafConfig, Lender, Role};
use loquax::fanout::Fanout;
use loquax::home::runner::{self, HomeClient};
use loquax::home::watch::{self, Fallback, WatchEvent};
use loquax::outbox::{Command, Outbox, OutboxEvent};
use loquax::store::Store;
use loquax::ui::browse::BrowseEvent;
use loquax::ui::history::StoreRead;
use loquax::ui::intent::{Intent, Messages, Switch, Target};
use loquax::ui::liveness::Liveness;
use loquax::ui::map::MapPin;
use loquax::ui::recording::{Effect, Haptic, Input, LevelTrack, Verb};
use loquax::ui::strings::{Lang, StrId, Text};
use loquax::ui::{chat, compose, scene, update, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;

use crate::links::{self, Book};
use crate::mailbox;
use crate::offline::Offline;
use crate::pull_line::{PullLine, Say};
use crate::schema::{self, Event, VERSION};

/// The folder under the app's received files (`loquax::file_save::DIR`)
/// the Browse tab's downloads are kept in.
pub const DOWNLOADS: &str = "browse";
/// The file under the app's directory that keeps the home node's address.
pub const SERVER_FILE: &str = "home-server";
/// How long a join, a send or a mark may take; a pull gets twice this.
pub const BUDGET: Duration = Duration::from_secs(60);

/// How old the mailbox's word on itself may be before the mailbox page
/// asks again (1642): a minute, so a device's "last there" moves while the
/// page is watched, and a page opened ten times asks once.
const ABOUT_FRESH_MS: u64 = 60_000;
/// How often the session takes what the engine said and runs the clock
/// while something is going on: the terminal's pump interval.
pub const PUMP: Duration = Duration::from_millis(100);

/// The name of the pump's OS thread (1496). Before, the pump was a task on
/// the runtime's two workers: each turn's synchronous SQLite commits held
/// a worker the node's and the runners' tasks needed (structure review
/// 2026-11-07, B4).
pub const PUMP_THREAD: &str = "loquax-pump";
/// The longest the pump sleeps while the session is idle (1176): no screen
/// in front, the watching link up, nothing sent, pulled or owed. Then
/// nothing the pump does is due before the pull schedule's next pull, and
/// whatever starts work wakes it at once (a request from Java, a runner's
/// end, the watching link's word). The ceiling is the link keepalive's
/// (`watch::LOOK`), so an idle phone's own timers wake it no more often
/// than its link keepalive does. Before 1176 the pump ran every 100 ms for
/// the life of the session.
pub const IDLE_PUMP: Duration = watch::LOOK;
/// The longest the pump sleeps while a position share runs (1273): its
/// shortest gap, so a report and the end are at most that late.
pub const SHARE_PUMP: Duration = Duration::from_secs(loquax::ui::share::MIN_GAP_S as u64);
/// How one pump turn folds the engine's events under the session's lock
/// (1446, 1464): [`loquax::fanout::SLICE`], the desktop shell's too. Every
/// event folded is a store commit with its fsyncs, and the first pull of
/// a big mailbox brings thousands: before 1446 one turn folded all of
/// them, 12.8 s on the host for 4000 entries, while
/// `ConversationsActivity.onResume` waited on the lock from the UI thread
/// and Android said the app was not responding. One event between two
/// looks at the clock and at the callers waiting for the lock (1464): with
/// eight a turn ran 60 to 80 ms past its budget on the host, and a command
/// that came in the middle waited for all of it.
pub const SLICE: loquax::fanout::Slice = loquax::fanout::SLICE;

thread_local! {
    /// This thread's calls of [`Session::lock`] and the engine's events the
    /// pump folded while they waited, summed (1610).
    static LOCK_WAITS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// How often this thread took the session's lock, and how many of the
/// engine's events the pump folded while it waited for it, since the
/// thread began (1610): the pump ends its turn after the event in hand and
/// starts none while a caller waits, so the second is at most the first,
/// however slow the host. For the facade test.
#[doc(hidden)]
pub fn lock_waits() -> (u64, u64) {
    LOCK_WAITS.with(std::cell::Cell::get)
}
/// How long the pump lets go of the lock between two slices of a backlog:
/// long enough for a screen woken from its wait on the lock to take it,
/// short against a [`SLICE`], so the history still comes at nine tenths of
/// the speed.
pub const BACKLOG_PUMP: Duration = Duration::from_millis(5);
/// How long a send runner waits for its bubble to be named.
const SETTLE: Duration = Duration::from_secs(5);
/// How often the TCP interface is looked at while the pump runs fast.
const LINK_EVERY_MS: u64 = 1_000;
/// Within how long of its mailbox going silent the phone says "not
/// connected" (1346): the notice (`Notice.java`) and the join screen's
/// line follow [`Event::Link`], which the pump sends when it sees the
/// TCP interface down. An idle connection to a silent peer is dropped by
/// the kernel after the device's TCP idle and one probe
/// (`InterfaceConfig::tcp_silent_peer_bound`, 242 s), the carrier change
/// (`leviculum_std::interfaces::carrier_changes`) wakes the pump out of its
/// idle sleep, and the pump looks at once. Before 1346 it was 26 s, and the idle pump could sleep
/// the drop away for up to [`IDLE_PUMP`].
pub const NOT_CONNECTED_WITHIN: Duration = Duration::from_secs(300);
/// How many of a peer's newest positions the contact screen lists.
const POSITIONS_SHOWN: usize = 10;
/// How many of the positions we shared the privacy page lists (1201).
pub const SHARED_SHOWN: usize = 100;

/// Why the session did not start, and what the join screen has to ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartError {
    pub need: Option<&'static str>,
    pub text: String,
    /// The words behind [`StartError::text`] for the screen's Details
    /// (1263): the mailbox's or the parser's own.
    pub details: Option<String>,
}

impl StartError {
    fn other(text: impl Into<String>) -> Self {
        Self {
            need: None,
            text: text.into(),
            details: None,
        }
    }

    /// One plain sentence of the catalogue in `lang`, what went wrong in
    /// `details` (1263, finding 8).
    fn said(need: Option<&'static str>, words: StrId, lang: Lang, details: Option<String>) -> Self {
        Self {
            need,
            text: words.text(lang).to_string(),
            details: details.filter(|details| !details.is_empty()),
        }
    }
}

/// The compose field the model needs a type for. Nobody types into it: the
/// app's own text field sends with [`UiEvent::Write`].
#[derive(Debug, Default, Clone)]
pub struct Line(String);

impl InputBuffer for Line {
    fn value(&self) -> &str {
        &self.0
    }
    fn cursor(&self) -> usize {
        self.0.chars().count()
    }
    fn edit(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
    fn newline(&mut self) {
        self.0.push('\n');
    }
    fn set_cursor(&mut self, _at: usize) {}
    fn set_value(&mut self, text: &str) {
        self.0 = text.to_string();
    }
}

/// A runner's end, folded by the pump.
enum Finished {
    Send(UiEvent),
    /// A pull's end, with the store's conversations read off the lock when
    /// it imported history (1464).
    Pull(
        runner::PullEnd,
        Option<Result<Vec<loquax::ui::history::Restored>, String>>,
    ),
    /// Another runner's answer, the event the model folds (1307).
    Event(UiEvent),
}

/// Everything the lock guards.
struct Inner {
    model: UiModel<Line>,
    store: Store,
    attached: Attached,
    fanout: Fanout,
    client: HomeClient,
    home: PathBuf,
    offset: i32,
    /// The phone's language: the scene's words are said in it (1217).
    lang: Lang,
    sends: VecDeque<([u8; 16], String, Vec<Field>)>,
    /// What the Browse tab asked of the platform (1337), oldest first: the
    /// fetches, downloads and stores of lnomad's [`lnomad::Effect`]. The
    /// fetch through the mailbox's node is slice 3 of
    /// `docs/src/concepts/lnomad-everywhere.md`; until it lands they wait
    /// here and the page says it is loading.
    browse_owed: Vec<lnomad::Effect>,
    /// The fetch they go to, over the phone's own node (1337 pass two).
    fetching: Option<loquax::browse_fetch::Fetching>,
    /// What the tab hands Java once, with the next [`schema::Query::Browse`]:
    /// a text for the clipboard, and a downloaded file's path and name.
    browse_copy: Option<String>,
    browse_file: Option<(String, String)>,
    /// The Rooms' hub link (1384), where the config names a hub.
    rooming: Option<loquax::rooms_link::Linking>,
    sending: bool,
    pull_owed: bool,
    pulling: bool,
    /// The running `/pages` ask (1579), one at a time.
    pages_asking: Option<tokio::task::JoinHandle<()>>,
    /// Whether the user asked for the pull owed (Refresh), and what the
    /// status line is told of the running one (1136).
    pull_asked: bool,
    pull_line: PullLine,
    /// Whether the list says "no connection" (1448): the watching link
    /// stayed down.
    offline: Offline,
    finished_tx: Sender<Finished>,
    finished_rx: Receiver<Finished>,
    events: Sender<Event>,
    runtime: tokio::runtime::Handle,
    origin: std::time::Instant,
    /// Something the screen shows changed since the last `changed`.
    dirty: bool,
    /// The last pump turn stopped at its [`SLICE`], or for a caller waiting
    /// for the lock (1464), with events still waiting in the engine's
    /// queue (1446).
    backlog: bool,
    /// How many callers of [`Session::lock`] wait for the lock now: the
    /// pump ends its turn after the event it is folding (1464).
    waiting: Arc<std::sync::atomic::AtomicUsize>,
    /// The engine's events the pump folded under the lock so far: what a
    /// caller of [`Session::lock`] waited behind is the count's step over
    /// its wait (1610).
    folded: Arc<std::sync::atomic::AtomicU64>,
    /// The list screens as of the pump's last change (1464), which the UI
    /// thread reads while the pump holds the lock.
    scene: Arc<Mutex<Scene>>,
    /// [`schema::Event::Changed::counts`] as the last scene made it.
    counts: u64,
    gone: bool,
    /// Whether the TCP interface to the home node was up at the last look,
    /// none before the first, and when that look was.
    link: Option<bool>,
    link_looked: Option<u64>,
    /// When the watching link to the mailbox last changed, and when it was
    /// last seen up, unix seconds (1642): the mailbox page's "since" and
    /// "last contact". The watching link, not the TCP interface (1647,
    /// walk 6, finding 37: the page counted "connected for" through a
    /// mailbox restart, which the TCP connection to the lnsd in front of
    /// it never saw). `None` before the link said anything.
    link_since: Option<i64>,
    last_contact: Option<i64>,
    /// What the mailbox said of itself (1642, `/about`), and whether an
    /// ask runs and when the last one began (the pump's clock).
    about: Arc<Mutex<AboutAsk>>,
    /// Whether the last turn skipped its look at the TCP interface.
    link_owed: bool,
    /// Wakes the pump out of an idle sleep (1176).
    wake: Arc<tokio::sync::Notify>,
    /// The watching link's word (`loquax::home::watch`), whether it is up,
    /// whether a screen of the app is in front, and the pull every minute
    /// while it is and the link is down.
    watch: tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    watching: bool,
    front: bool,
    fallback: Fallback,
    /// loquax's links to the phone's address book (1139).
    book: Book,
    /// Whether a linked peer's thumbnail is kept, and its path (1496):
    /// the lists asked the file system once per linked row per list made,
    /// 300 stats for the real fixture; the link and the unlink, which keep
    /// and drop the file, drop the entry.
    photos: std::cell::RefCell<std::collections::HashMap<[u8; 16], Option<String>>>,
    /// How often [`Inner::photo`] asked the file system, for the facade.
    photo_stats: std::cell::Cell<u64>,
    /// The lists' rows as last made, by peer, with the stamp they were
    /// made under (1595, [`Inner::row_stamp`]): a key that changed one
    /// conversation makes that row again and reuses the other 299; before,
    /// every request made all of them, twice with the pump's echo.
    row_cache:
        std::cell::RefCell<std::collections::HashMap<[u8; 16], (u64, schema::ConversationRow)>>,
    /// The request's lists stand for what the model's state was when it
    /// made them (1595): the pump's next turn does not make them again
    /// for the request's own change, only for its own or a revision moved.
    request_shown: bool,
    /// Whether the pump's last turn changed something of its own (1600):
    /// an engine event, a pull's end, a setting saved. A bar on the
    /// request's echo reads it to tell an echo from the turn's own work.
    turn_own: bool,
    /// The thread the pump's last turn ran on (1496), for the facade.
    pump_thread: Option<String>,
    /// The peers added by a code while this session runs (walk finding
    /// 15): their chat does not ask to save them to the phone's contacts,
    /// the user just named them.
    just_added: std::collections::BTreeSet<[u8; 16]>,
    /// The contact keys the home node took (1187), for the facade test.
    keys_told: Arc<std::sync::atomic::AtomicUsize>,
    /// What the recorder is to do (1276, `UiEffect::Recording`), by peer,
    /// until [`schema::Query::Recording`] hands it out.
    recorder: Vec<([u8; 16], Effect)>,
    /// The platform's word on the network (1298), whether the line says
    /// "Reconnecting…" now, and the re-dials it has seen begin.
    network: loquax::home::rendezvous::Network,
    reconnecting: bool,
    redials_seen: usize,
    /// What the device link cost since the last `WAKE_SUMMARY` (1346).
    costs: DeviceCosts,
}

/// The two list screens' answers, encoded, as the pump left them at the
/// end of its last turn that changed something, or as the last caller who
/// found the lock free saw them (1464). The UI thread asks for the list once
/// per event (the Pixel's profile: 992 times in 44 s of a first pull), and
/// while the pump folds that pull it holds the lock 50 ms at a time: an
/// answer that waited for it froze the screen 80 to 100 ms per event. A
/// list asked while the lock is held is this one: never older than the last
/// `changed` the screens were told.
#[derive(Default)]
struct Scene {
    conversations: Option<Vec<u8>>,
    archived: Option<Vec<u8>>,
    /// The model's lists revision the two show (1491,
    /// `UiModel::lists_revision`): the pump makes them again when it
    /// moved, and only then. A caller never makes them: the
    /// UI thread takes them as they are.
    at: Option<u64>,
    /// How often they were made, for the facade's idle bar.
    builds: u64,
}

/// [`schema::Event::Changed::counts`] of `listed` (1464): every row's
/// peer, unread count, muted and sharing flag, hashed.
fn counts(listed: &schema::Conversations) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for row in &listed.conversations {
        (&row.peer, row.unread, row.muted, row.sharing).hash(&mut hasher);
    }
    hasher.finish()
}

/// Which of the two lists of [`Scene`].
#[derive(Clone, Copy)]
enum Listed {
    Conversations,
    Archived,
}

/// The phone's side of the `WAKE_SUMMARY` line (1346): the pump's turns,
/// the links to the home that came up (told by [`relay_node`]), their
/// keepalive round trips, and the bytes on the connections to the home or
/// rendezvous, read with the TCP interface's look.
#[derive(Default)]
struct DeviceCosts {
    window: loquax::home::keepalive::Costs,
    since_ms: u64,
    /// Each link to the home this phone opened, and its keepalive total at
    /// the last reading.
    links: Vec<(leviculum_core::LinkId, u64)>,
    /// Each interface's byte total at the last reading.
    bytes: std::collections::BTreeMap<usize, u64>,
    opened: Option<tokio::sync::mpsc::UnboundedReceiver<leviculum_core::LinkId>>,
}

impl DeviceCosts {
    /// Read the totals the node keeps.
    fn read(&mut self, node: &leviculum_std::driver::ReticulumNode) {
        use loquax::home::keepalive::growth;
        if let Some(opened) = &mut self.opened {
            while let Ok(link) = opened.try_recv() {
                self.window.links += 1;
                self.links.push((link, 0));
            }
        }
        let mut keepalives = 0;
        self.links
            .retain_mut(|(link, last)| match node.link_keepalive(link) {
                Some((_, total)) => {
                    keepalives += growth(last, total);
                    true
                }
                None => false,
            });
        self.window.keepalives += keepalives;
        for interface in node.interface_stats() {
            if interface.is_local_client {
                continue;
            }
            let last = self.bytes.entry(interface.interface_id.0).or_default();
            self.window.bytes += growth(last, interface.rx_bytes + interface.tx_bytes);
        }
    }

    /// The line, when the window is over: and a fresh window.
    fn summary(&mut self, now_ms: u64) -> Option<String> {
        if now_ms.saturating_sub(self.since_ms) < loquax::home::keepalive::SUMMARY_EVERY_MS {
            return None;
        }
        self.since_ms = now_ms;
        Some(loquax::home::keepalive::device_line(&std::mem::take(
            &mut self.window,
        )))
    }
}

/// A running session.
pub struct Session {
    /// The model and everything it folds, behind a `parking_lot` mutex
    /// (1496): fair handoff when a screen's call waits while the pump
    /// unlocks and locks again, no poisoning to unwrap.
    inner: Arc<parking_lot::Mutex<Inner>>,
    /// The pump's own OS thread, [`PUMP_THREAD`] (1496), joined by the
    /// stop.
    pump: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// [`Inner::waiting`].
    waiting: Arc<std::sync::atomic::AtomicUsize>,
    /// [`Inner::folded`].
    folded: Arc<std::sync::atomic::AtomicU64>,
    /// [`Inner::scene`].
    scene: Arc<Mutex<Scene>>,
    /// Wakes the pump when a request may have started work (1176).
    wake: Arc<tokio::sync::Notify>,
    /// The pump makes no turn while set ([`Session::with_pump_held`]).
    held: Arc<std::sync::atomic::AtomicBool>,
    events: Mutex<Receiver<Event>>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    /// The mailbox, when it is on this phone.
    here: Mutex<Option<mailbox::Here>>,
    /// The device "Link a device" waits for (1264), when one was asked.
    linking: Mutex<Option<mailbox::Linking>>,
    /// The new phone "Move to a new phone" waits for (1281).
    moving: Mutex<Option<mailbox::Linking>>,
    address: [u8; 16],
    server: String,
    /// The list the leaf dials one at a time, [`Session::server`] first.
    servers: Vec<String>,
}

impl Session {
    /// Start: on a phone that is no client yet, join with the code first.
    /// Blocks for as long as the join takes, up to [`BUDGET`].
    /// A code with the address behind it (`lqxh1...@host:port`) brings the
    /// server along; a server given beside it wins.
    pub fn start(start: &schema::Start) -> Result<Self, StartError> {
        let home = PathBuf::from(&start.dir);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| StartError::other(format!("no runtime: {error}")))?;
        if mailbox::has_moved(&home) {
            // Walk finding 24 (1281): the mailbox went to the new phone,
            // and this one sends nothing until its owner says otherwise.
            if !start.take_back {
                return Err(StartError::said(
                    Some("moved"),
                    StrId::MovedAway,
                    Lang::of_tag(start.lang.as_deref().unwrap_or_default()),
                    None,
                ));
            }
            mailbox::take_back(&home).map_err(StartError::other)?;
        }
        // The platform's word on the network (1298), heard by the leaf's
        // rotation and the phone's own mailbox's alike.
        let network = loquax::home::rendezvous::Network::new();
        // The node the Browse tab fetches through (1337 pass two) and the
        // Rooms link to their hub over (1384): the phone's mailbox's when
        // it is here, else the leaf's.
        let (lend, lends) = tokio::sync::mpsc::unbounded_channel();
        let (rooms_lend, rooms_lends) = tokio::sync::mpsc::unbounded_channel();
        let (start, here) = if start.here || mailbox::is_here(&home) {
            let lending = Vec::from([
                (Lender::Browse, lend.clone()),
                (Lender::Rooms, rooms_lend.clone()),
            ]);
            let (start, here) =
                Self::start_here(start, &home, runtime.handle(), &network, lending)?;
            (start, Some(here))
        } else {
            (start.clone(), None)
        };
        let start = &start;
        let lang = Lang::of_tag(start.lang.as_deref().unwrap_or_default());
        let client = loquax::home::client::load_client(&home).map_err(StartError::other)?;
        let mut carried = Vec::new();
        let code = match (&client, start.code.as_deref().map(str::trim)) {
            (_, Some(code)) if !code.is_empty() => {
                let (code, list) = loquax::home::code::HomeCode::parse_with_rendezvous(code)
                    .map_err(|error| {
                        StartError::said(
                            Some("code"),
                            StrId::JoinNotMailbox,
                            lang,
                            Some(format!("That is not a mailbox code: {error}")),
                        )
                    })?;
                carried = list;
                Some(code)
            }
            (Some(_), _) => None,
            (None, _) => {
                return Err(StartError::said(
                    Some("code"),
                    StrId::JoinNoCode,
                    lang,
                    Some("This phone reads no mailbox yet.".to_string()),
                ))
            }
        };
        if let (Some(key), Some(code)) = (&client, &code) {
            if *key != code.public_key {
                return Err(StartError::said(None, StrId::JoinOtherMailbox, lang, None));
            }
        }
        // The keepalive of the connection to the mailbox: the same `tcp_*`
        // keys as an lnsd section (1176), from this app's config file, and
        // the rendezvous keys (1196) beside them.
        let config =
            loquax::config::load(&home).map_err(|error| StartError::other(error.to_string()))?;
        let servers =
            dial_list(start.server.as_deref(), &config, carried, &home).ok_or_else(|| {
                StartError::said(
                    Some("server"),
                    StrId::JoinWhere,
                    lang,
                    Some("No server: the code carries none and none was typed.".to_string()),
                )
            })?;
        for server in &servers {
            one_server(server).map_err(|text| {
                StartError::said(Some("server"), StrId::JoinBadWhere, lang, Some(text))
            })?;
        }
        let server = servers.first().cloned().unwrap_or_default();
        let public_key = match (&code, client) {
            (Some(code), _) => code.public_key,
            (None, Some(key)) => key,
            (None, None) => return Err(StartError::other("no home node")),
        };
        let address = loquax::home::lxmf_address(&public_key).map_err(StartError::other)?;
        let device = loquax::identity::load_or_create(
            &home.join(loquax::home::client::DEVICE_IDENTITY_FILE),
        )
        .map_err(|error| StartError::other(error.to_string()))?;
        let storage = home.join("storage");
        std::fs::create_dir_all(&storage)
            .map_err(|error| StartError::other(format!("{}: {error}", storage.display())))?;

        let tcp = config.tcp;
        let home_destination =
            loquax::home::destination_hash(&public_key).map_err(StartError::other)?;
        let attached = runtime.block_on(attach_leaf(
            LeafConfig {
                servers: servers.clone(),
                home_destination: Some(home_destination),
                timing: loquax::home::rendezvous::Timing::default(),
                storage_dir: storage,
                identity: device.clone(),
                display_name: Vec::new(),
                notices: loquax::effects::Terminal::new(home.clone()),
                tcp,
                // The phone's own mailbox is on the loopback address, which
                // no network change touches: its rotation hears none.
                network: match here {
                    Some(_) => loquax::home::rendezvous::Network::new(),
                    None => network.clone(),
                },
            },
            Role::Client { address },
        ));
        let attached = attached.map_err(|error| StartError::other(error.to_string()))?;
        // The mailbox here lends its own node; the screens' leaf reaches
        // nothing past it. A mailbox elsewhere is reached through a
        // rendezvous, which carries the leaf's links on.
        let browse_identity = match &here {
            Some(_) => mailbox::identity(&home).map_err(StartError::other)?,
            None => {
                let _ = lend.send(attached.lend());
                let _ = rooms_lend.send(attached.lend_to(Lender::Rooms));
                device.clone()
            }
        };
        drop(lend);
        drop(rooms_lend);
        let client = HomeClient {
            node: attached.node(),
            public_key,
            device,
        };
        if let Some(code) = &code {
            let name = start.name.clone().unwrap_or_else(|| "phone".to_string());
            let joined = runtime.block_on(loquax::home::client::join(
                &client.node,
                &code.public_key,
                &client.device,
                &code.token,
                &name,
                BUDGET,
            ));
            if let Err(error) = joined {
                drop(client);
                runtime.block_on(attached.stop());
                let need = match error {
                    loquax::home::client::HomeError::NoPath => Some("server"),
                    loquax::home::client::HomeError::Refused(_) => Some("code"),
                    _ => None,
                };
                return Err(StartError::said(
                    need,
                    join_words(&error),
                    lang,
                    Some(join_details(&error, &servers.join(", "))),
                ));
            }
            loquax::home::client::save_client(&home, &code.public_key)
                .map_err(StartError::other)?;
        }
        // The phone's own mailbox listens on another port at every start.
        if here.is_none() {
            save_servers(&home, &servers).map_err(StartError::other)?;
        }

        let (events_tx, events_rx) = channel();
        let (watch_tx, watch_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake = Arc::new(tokio::sync::Notify::new());
        // Subscribed before the watch opens its first link, so its
        // establishment is counted.
        let node_events = client.node.subscribe_events();
        let watching = watch::Watch {
            node: client.node.clone(),
            public_key,
            device: client.device.clone(),
            home: home.clone(),
        };
        let inner = Inner::new(
            &home,
            address,
            attached,
            client,
            start.offset,
            Lang::of_tag(start.lang.as_deref().unwrap_or_default()),
            start.camera,
            events_tx,
            runtime.handle().clone(),
            watch_rx,
            Arc::clone(&wake),
        )
        .map_err(StartError::other)?;
        // Ends with the runtime, or once the session's end of it is dropped.
        // The watch's word goes through a relay that wakes the pump, which
        // may be in an idle sleep (1176).
        let (heard_tx, heard_rx) = tokio::sync::mpsc::unbounded_channel();
        runtime.spawn(watch::run(watching, heard_tx));
        runtime.spawn(relay_watch(heard_rx, watch_tx, Arc::clone(&wake)));
        let (opened_tx, opened_rx) = tokio::sync::mpsc::unbounded_channel();
        runtime.spawn(relay_node(
            node_events,
            leviculum_core::DestinationHash::new(home_destination),
            opened_tx,
            Arc::clone(&wake),
        ));
        let mut inner = inner;
        inner.redials_seen = network.redials();
        inner.costs.opened = Some(opened_rx);
        inner.network = network.clone();
        // The Rooms (1384), as on the desktop: identified as the Browse
        // tab identifies, and shown by the mailbox's name when it is here.
        // The link runs with no hub too, idle until asked (1577): the
        // Advanced page chooses one while the app runs.
        {
            let nick = match &here {
                Some(_) => Some(mailbox::name(&home)).filter(|name| !name.is_empty()),
                None => None,
            };
            inner.rooming = Some(loquax::rooms_link::start(
                runtime.handle(),
                loquax::rooms_link::Options {
                    lends: rooms_lends,
                    identity: browse_identity.clone(),
                    nick,
                },
                Arc::clone(&wake),
            ));
        }
        inner.fetching = Some(loquax::browse_fetch::start(
            runtime.handle(),
            loquax::browse_fetch::Options {
                lends,
                identity: browse_identity,
                identify: home.join(loquax::browse_fetch::IDENTIFY_FILE),
            },
            Arc::clone(&wake),
        ));
        let waiting = Arc::clone(&inner.waiting);
        let folded = Arc::clone(&inner.folded);
        let scene = Arc::clone(&inner.scene);
        // The lists exist before any screen asks (1491): a screen never
        // makes them, so the first is made here, before the pump runs.
        inner.show_scene();
        let inner = Arc::new(parking_lot::Mutex::new(inner));
        let pumped = Arc::clone(&inner);
        let pump_wake = Arc::clone(&wake);
        let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pump_held = Arc::clone(&held);
        let handle = runtime.handle().clone();
        // The pump on an OS thread of its own (1496), in the runtime's
        // context for its timers and what it spawns: its commits block
        // this thread, never a worker. It ends when the session is gone
        // (`Inner::pump` says false), which the stop sets and wakes it for.
        let pump = std::thread::Builder::new()
            .name(PUMP_THREAD.to_string())
            .spawn(move || {
                handle.block_on(async move {
                    let mut wait = PUMP;
                    loop {
                        tokio::select! {
                            () = tokio::time::sleep(wait) => {}
                            () = pump_wake.notified() => {}
                            // A re-dial for a network change began or
                            // ended: the line follows it even while the
                            // pump sleeps idle (1334).
                            () = network.turned() => {}
                        }
                        let mut inner = pumped.lock();
                        // Held (1587): no turn, read under the lock so a
                        // turn under way when the hold came has ended; a
                        // stop still ends the pump.
                        if pump_held.load(std::sync::atomic::Ordering::SeqCst) && !inner.gone {
                            wait = PUMP;
                            continue;
                        }
                        if inner.pump_thread.is_none() {
                            inner.pump_thread = std::thread::current().name().map(str::to_string);
                        }
                        if !inner.pump() {
                            return;
                        }
                        wait = inner.next_pump();
                    }
                })
            })
            .map_err(|error| StartError::other(format!("no pump thread: {error}")))?;
        let session = Self {
            inner,
            pump: Mutex::new(Some(pump)),
            waiting,
            folded,
            scene,
            wake,
            held,
            events: Mutex::new(events_rx),
            runtime: Mutex::new(Some(runtime)),
            here: Mutex::new(here),
            linking: Mutex::new(None),
            moving: Mutex::new(None),
            address,
            server,
            servers,
        };
        // What the home node holds since the last run, on open.
        session.lock().apply(UiEvent::Run(PaletteCommand::Fetch));
        Ok(session)
    }

    /// The mailbox on this phone: chosen and started on `runtime`, and the
    /// start as the screens' client makes it, to the mailbox's loopback
    /// server, with a code of its own while the screens are not joined.
    fn start_here(
        start: &schema::Start,
        home: &Path,
        runtime: &tokio::runtime::Handle,
        network: &loquax::home::rendezvous::Network,
        lends: loquax::home::daemon::Lends,
    ) -> Result<(schema::Start, mailbox::Here), StartError> {
        let lang = Lang::of_tag(start.lang.as_deref().unwrap_or_default());
        if let Some(text) = start.moving.as_deref() {
            let name = start.name.as_deref().unwrap_or("phone");
            mailbox::move_in(home, text, name, runtime).map_err(|failed| match failed {
                mailbox::MoveFailed::Code(details) => {
                    StartError::said(Some("code"), StrId::JoinNotMailbox, lang, Some(details))
                }
                mailbox::MoveFailed::HasAddress => {
                    StartError::said(None, StrId::MoveHasAddress, lang, None)
                }
                mailbox::MoveFailed::Move(error) => {
                    let need = match error {
                        loquax::home::client::HomeError::Refused(_) => Some("code"),
                        _ => None,
                    };
                    StartError::said(need, StrId::MoveFailed, lang, Some(error.to_string()))
                }
                mailbox::MoveFailed::Here(details) => StartError::other(details),
            })?;
        }
        if let Some(line) = start.restore.as_deref() {
            let passphrase = start.passphrase.as_deref().unwrap_or_default();
            // The guard (1281): a fresh restore of an address that answers
            // on another device right now warns first, since two live
            // copies lose messages on both.
            if !start.anyway && !home.join(mailbox::DIR).join("identity").exists() {
                let key = mailbox::backup_key(line, passphrase).map_err(|text| StartError {
                    need: Some("passphrase"),
                    text,
                    details: None,
                })?;
                if mailbox::answers_elsewhere(home, &key, mailbox::PROBE_BUDGET, runtime)
                    .map_err(StartError::other)?
                {
                    return Err(StartError::said(
                        Some("live"),
                        StrId::RestoreLive,
                        lang,
                        None,
                    ));
                }
            }
            mailbox::restore(home, line, passphrase).map_err(|text| StartError {
                need: Some("passphrase"),
                text,
                details: None,
            })?;
        }
        let joined = loquax::home::client::load_client(home).map_err(StartError::other)?;
        if joined.is_some() && !mailbox::is_here(home) {
            return Err(StartError::other(
                "This phone already reads a mailbox on a computer.",
            ));
        }
        let public_key = mailbox::choose_here(home).map_err(StartError::other)?;
        if joined.is_some_and(|key| key != public_key) {
            return Err(StartError::said(
                None,
                StrId::JoinOtherMailbox,
                Lang::of_tag(start.lang.as_deref().unwrap_or_default()),
                None,
            ));
        }
        let code = match joined {
            Some(_) => None,
            None => Some(
                mailbox::admission(home)
                    .map_err(StartError::other)?
                    .encode(),
            ),
        };
        let here = mailbox::Here::start(home, runtime, network.clone(), lends)
            .map_err(StartError::other)?;
        let start = schema::Start {
            server: Some(here.server()),
            code,
            name: Some(
                start
                    .name
                    .clone()
                    .unwrap_or_else(|| "this phone".to_string()),
            ),
            ..start.clone()
        };
        Ok((start, here))
    }

    /// This phone's own address as [`schema::Query::Me`] answers it.
    fn me(&self) -> Result<schema::Me, String> {
        let home = self.lock().home.clone();
        let here = self.is_here();
        // One contact code everywhere (1263, finding 1): the profile's, so
        // a friend's scanner takes the first start's QR as well.
        let lxma = loquax::lxma::encode(&self.lock().client.public_key)?;
        let (qr_size, modules) = loquax::home::qr::verbatim_modules(&lxma)?;
        Ok(schema::Me {
            v: VERSION,
            address: loquax::address::to_hex(&self.address),
            fingerprint: fingerprint(&self.address),
            lxma,
            qr_size,
            qr: schema::Blob(modules.into_iter().map(u8::from).collect()),
            name: if here {
                mailbox::name(&home)
            } else {
                String::new()
            },
            here,
        })
    }

    /// The profile behind our avatar ([`schema::Query::Profile`], 1187).
    fn profile(&self) -> Result<schema::Profile, String> {
        let (home, public_key, initials) = {
            let inner = self.lock();
            (
                inner.home.clone(),
                inner.client.public_key,
                inner.initials(&self.address),
            )
        };
        let here = self.is_here();
        let lxma = loquax::lxma::encode(&public_key)?;
        let (qr_size, modules) = loquax::home::qr::verbatim_modules(&lxma)?;
        let identity = leviculum_core::Identity::from_public_key_bytes(&public_key)
            .map_err(|error| format!("the mailbox's key: {error:?}"))?;
        let since = std::fs::metadata(home.join(loquax::home::client::CLIENT_FILE))
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |at| i64::try_from(at.as_secs()).unwrap_or(0));
        let propagation_node = match here {
            true => loquax::config::load(&home.join(mailbox::DIR))
                .ok()
                .and_then(|config| config.propagation_node)
                .map(|node| loquax::address::to_hex(&node))
                .unwrap_or_default(),
            false => String::new(),
        };
        let name = match here {
            true => mailbox::name(&home),
            false => String::new(),
        };
        let words = self.profile_words(&home, here, &name, &lxma, (qr_size, modules.clone()));
        Ok(schema::Profile {
            v: VERSION,
            name,
            name_editable: here,
            address: loquax::address::to_hex(&self.address),
            initials,
            lxma,
            qr_size,
            qr: schema::Blob(modules.into_iter().map(u8::from).collect()),
            here,
            mailbox: if here {
                String::new()
            } else {
                server_host(&self.server).to_string()
            },
            since,
            identity: loquax::address::to_hex(identity.hash()),
            home_node: loquax::address::to_hex(&loquax::home::destination_hash(&public_key)?),
            rendezvous: self
                .rendezvous()
                .iter()
                .map(|(server, up)| format!("{} {server}", if *up { '●' } else { '○' }))
                .collect::<Vec<_>>()
                .join("\n"),
            propagation_node,
            version: loquax::VERSION.to_string(),
            words,
        })
    }

    /// The core's profile scene (1443) in the reader's words: the phone's
    /// facts told to the model (the name, where the mailbox is and how the
    /// phone reaches it, the devices, the code), then the scene the window
    /// and the terminal draw, resolved for the activity.
    fn profile_words(
        &self,
        home: &std::path::Path,
        here: bool,
        name: &str,
        lxma: &str,
        qr: (u32, Vec<bool>),
    ) -> schema::ProfileWords {
        let facts = self.profile_facts(home, here, name);
        let mut inner = self.lock();
        inner.model.own_code = Some(loquax::ui::OwnCode::of(lxma.to_string(), Some(qr)));
        inner.apply(UiEvent::ProfileFacts(Box::new(facts)));
        profile_words(&inner.model, inner.lang)
    }

    /// What the phone knows of the reader that the model does not (1443,
    /// 1642): the name, where the mailbox is and how the phone reaches it,
    /// what the mailbox said of itself, the devices.
    fn profile_facts(
        &self,
        home: &std::path::Path,
        here: bool,
        name: &str,
    ) -> loquax::ui::profile::ProfileFacts {
        use loquax::ui::mailbox_page::Reach;
        use loquax::ui::profile::{Device, Mailbox, ProfileFacts, Rung};
        let rendezvous = self.rendezvous();
        let relayed = rendezvous.iter().any(|(_, up)| *up);
        let (link, watching, heard, since, last_contact, entry, told, device) = {
            let inner = self.lock();
            // The mailbox announces this identity under the name it keeps
            // (1447): what the profile calls it, never the server the
            // phone reaches it through.
            let heard = inner
                .model
                .names
                .announced(&self.address)
                .map(str::to_string);
            let entry = inner
                .attached
                .rendezvous()
                .map(|state| state.entry)
                .filter(|entry| !entry.is_empty());
            let told = lock_about(&inner.about).told.clone();
            (
                inner.link,
                inner.link_since.map(|_| inner.watching),
                heard,
                inner.link_since,
                inner.last_contact,
                entry,
                told,
                *inner.client.device.hash(),
            )
        };
        let mailbox = match here {
            true => Mailbox::Here,
            false => Mailbox::Away {
                name: heard,
                rung: relayed.then_some(Rung::Relayed),
                word: link.map(|up| match up {
                    true => loquax::home::board::Word::Connected,
                    false => loquax::home::board::Word::Unreachable,
                }),
            },
        };
        let others = match here {
            true => mailbox::devices(home).unwrap_or_default(),
            false => Vec::new(),
        };
        let devices = std::iter::once(Device {
            name: String::new(),
            this: true,
        })
        .chain(others.into_iter().map(|name| Device { name, this: false }))
        .collect();
        ProfileFacts {
            name: name.to_string(),
            mailbox,
            devices,
            told,
            reach: Reach {
                up: watching,
                since,
                last_contact,
                entry,
                dials: !self.servers.is_empty(),
            },
            device: Some(device),
            version: loquax::VERSION.to_string(),
        }
    }

    /// The mailbox page of the settings (1642, [`schema::Query::MailboxPage`]):
    /// the facts told to the model, the mailbox asked again for its own
    /// word when the last ask is older than a minute, and the core's page
    /// resolved. Java asks again every few seconds while the page is up,
    /// so the answer of the ask begun here shows on the next look.
    fn mailbox_page(&self) -> Result<schema::MailboxPage, String> {
        let home = self.lock().home.clone();
        let here = self.is_here();
        let name = match here {
            true => mailbox::name(&home),
            false => String::new(),
        };
        let facts = self.profile_facts(&home, here, &name);
        let mut inner = self.lock();
        inner.model.now = Some(wall_now(inner.offset));
        if !here {
            inner.ask_about(ABOUT_FRESH_MS);
        }
        inner.apply(UiEvent::ProfileFacts(Box::new(facts)));
        Ok(mailbox_page_words(&inner.model, inner.lang))
    }

    /// A contact from its contact code ([`schema::Request::AddContact`],
    /// 1187): its name kept when one was given, its key handed to the home
    /// node in the background, and its conversation open. A home node that
    /// cannot take the key now is said as a notice: the contact is there,
    /// and the first message waits for its announce.
    fn add_contact(&self, lxma: &str, name: Option<&str>) -> Result<(), String> {
        let mut inner = self.lock();
        let code = loquax::code::read(lxma).map_err(|why| why.say(inner.lang).to_string())?;
        let (peer, key) = match code {
            loquax::code::Code::Contact(contact) => (contact.address, Some(contact.public_key)),
            // An old code, the address alone (1263): the contact is there,
            // the first message waits for its announce.
            loquax::code::Code::Address(address) => (address, None),
            loquax::code::Code::Mailbox(_) => {
                return Err(StrId::CodeIsMailbox.text(inner.lang).to_string())
            }
        };
        inner.just_added.insert(peer);
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
            inner.apply(UiEvent::Rename {
                peer,
                name: name.to_string(),
            });
        }
        let Some(contact_key) = key else {
            inner.apply(UiEvent::Open { peer });
            inner.dirty = true;
            return Ok(());
        };
        let node = inner.client.node.clone();
        let public_key = inner.client.public_key;
        let device = inner.client.device.clone();
        let events = inner.events.clone();
        let keys_told = Arc::clone(&inner.keys_told);
        inner.runtime.spawn(async move {
            let told =
                loquax::home::client::key(&node, &public_key, &device, &peer, &contact_key, BUDGET)
                    .await;
            if told.is_ok() {
                keys_told.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            if let Err(error) = told {
                tracing::warn!("key for {}: {error}", loquax::address::to_hex(&peer));
                let _ = events.send(Event::Notice {
                    v: VERSION,
                    text: "Your mailbox did not take the contact's key yet: the first \
                           message goes out once the contact is heard."
                        .to_string(),
                });
            }
        });
        inner.apply(UiEvent::Open { peer });
        inner.dirty = true;
        Ok(())
    }

    /// The address's backup ([`schema::Query::Backup`]), when the mailbox is
    /// on this phone: a reader of a mailbox elsewhere holds no address.
    fn backup(&self, passphrase: &str) -> Result<schema::Backup, String> {
        if !self.is_here() {
            return Err("the address lives in the mailbox, not on this phone".to_string());
        }
        let home = self.lock().home.clone();
        let line = mailbox::backup(&home, passphrase)?;
        let (qr_size, modules) = loquax::home::qr::modules(&line)?;
        Ok(schema::Backup {
            v: VERSION,
            line,
            qr_size,
            qr: schema::Blob(modules.into_iter().map(u8::from).collect()),
        })
    }

    /// "Link a device" (1264, [`schema::Query::LinkDevice`]): a join code
    /// of the mailbox on this phone, kept as the one [`Session::linked`]
    /// watches.
    fn link_device(&self) -> Result<schema::LinkCode, String> {
        if !self.is_here() {
            let lang = self.lock().lang;
            return Err(StrId::LinkOnComputer.text(lang).to_string());
        }
        let home = self.lock().home.clone();
        let (admission, linking) = mailbox::link_code(&home)?;
        let code = admission.text();
        let (qr_size, modules) = loquax::home::qr::modules(&code)?;
        let hours = linking.expires_at.saturating_sub(admission.issued_at) / 3600;
        let expires_at = linking.expires_at;
        *self.linking() = Some(linking);
        Ok(schema::LinkCode {
            v: VERSION,
            code,
            qr_size,
            qr: schema::Blob(modules.into_iter().map(u8::from).collect()),
            hours: u32::try_from(hours).unwrap_or(u32::MAX),
            expires_at,
        })
    }

    /// Whether a device came with the newest link code
    /// ([`schema::Query::Linked`]).
    fn linked(&self) -> Result<schema::Linked, String> {
        let Some(linking) = self.linking().clone() else {
            return Ok(schema::Linked {
                v: VERSION,
                state: "none".to_string(),
                name: String::new(),
            });
        };
        let home = self.lock().home.clone();
        let (state, name) = match mailbox::linked(&home, &linking)? {
            mailbox::Linked::Waiting => ("waiting", String::new()),
            mailbox::Linked::Linked(name) => ("linked", name),
            mailbox::Linked::Over => ("over", String::new()),
        };
        Ok(schema::Linked {
            v: VERSION,
            state: state.to_string(),
            name,
        })
    }

    /// "Move to a new phone" (1281, [`schema::Query::MovePhone`]): a code
    /// for the new phone, kept as the one [`Session::moving_state`]
    /// watches.
    fn move_phone(&self) -> Result<schema::LinkCode, String> {
        if !self.is_here() {
            let lang = self.lock().lang;
            return Err(StrId::MoveOnComputer.text(lang).to_string());
        }
        let home = self.lock().home.clone();
        let (admission, linking) = mailbox::move_code(&home)?;
        let code = admission.text();
        let (qr_size, modules) = loquax::home::qr::modules(&code)?;
        let hours = linking.expires_at.saturating_sub(admission.issued_at) / 3600;
        let expires_at = linking.expires_at;
        *self.moving_code() = Some(linking);
        Ok(schema::LinkCode {
            v: VERSION,
            code,
            qr_size,
            qr: schema::Blob(modules.into_iter().map(u8::from).collect()),
            hours: u32::try_from(hours).unwrap_or(u32::MAX),
            expires_at,
        })
    }

    /// Where the newest move code stands ([`schema::Query::Moving`]).
    fn moving_state(&self) -> Result<schema::Linked, String> {
        let Some(linking) = self.moving_code().clone() else {
            return Ok(schema::Linked {
                v: VERSION,
                state: "none".to_string(),
                name: String::new(),
            });
        };
        let home = self.lock().home.clone();
        let (state, name) = match mailbox::moving(&home, &linking)? {
            mailbox::Moving::Waiting => ("waiting", String::new()),
            mailbox::Moving::Moved(name) => ("moved", name),
            mailbox::Moving::Over => ("over", String::new()),
        };
        Ok(schema::Linked {
            v: VERSION,
            state: state.to_string(),
            name,
        })
    }

    fn moving_code(&self) -> MutexGuard<'_, Option<mailbox::Linking>> {
        self.moving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn linking(&self) -> MutexGuard<'_, Option<mailbox::Linking>> {
        self.linking
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether the mailbox is on this phone.
    pub fn is_here(&self) -> bool {
        self.here().is_some()
    }

    /// The name recipients see, when the mailbox is on this phone: kept,
    /// and the mailbox started again so its next announce carries it.
    pub fn rename(&self, name: &str) -> Result<(), String> {
        let home = self.lock().home.clone();
        mailbox::set_name(&home, name)?;
        let handle = self.handle().ok_or("the session has ended")?;
        let mut here = self.here();
        match here.as_mut() {
            Some(here) => handle.block_on(here.restart(&handle)),
            None => Err("the mailbox is not on this phone".to_string()),
        }
    }

    fn here(&self) -> MutexGuard<'_, Option<mailbox::Here>> {
        self.here
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn handle(&self) -> Option<tokio::runtime::Handle> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|runtime| runtime.handle().clone())
    }

    /// The home node's LXMF address, this phone's own.
    pub fn address(&self) -> [u8; 16] {
        self.address
    }

    /// The server the session talks to: the first of its list.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// The servers the session dials one at a time, in order, each with
    /// whether its connection is up now: the rendezvous list Advanced
    /// shows with the active entry marked (1196).
    pub fn rendezvous(&self) -> Vec<(String, bool)> {
        let state = self.lock().attached.rendezvous().unwrap_or_default();
        self.servers
            .iter()
            .map(|server| (server.clone(), state.up && state.entry == *server))
            .collect()
    }

    /// Whether the TCP interface to the home node is up now.
    pub fn link(&self) -> bool {
        link_up(&self.lock().client.node)
    }

    /// Whether the session ended by itself: the engine is gone, and only a
    /// fresh start helps.
    pub fn is_over(&self) -> bool {
        let mailbox_gone = self
            .here
            .try_lock()
            .is_ok_and(|here| here.as_ref().is_some_and(mailbox::Here::is_over));
        self.lock().gone || mailbox_gone
    }

    /// Cut the session off its engine, as the engine's death does; the
    /// next pump finds it gone. For the facade test.
    #[doc(hidden)]
    pub fn simulate_engine_death(&self) {
        self.lock().attached.outbox.sever();
    }

    /// How many contact keys the home node took. For the facade test.
    #[doc(hidden)]
    pub fn keys_told(&self) -> usize {
        self.lock()
            .keys_told
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// What the model built and the store read and committed since the
    /// last event or [`Session::reset_counters`] (1489,
    /// `loquax::ui::counters`); `None` in a release build without
    /// loquax's `counters` feature. For the facade's bars.
    #[doc(hidden)]
    pub fn counters(&self) -> Option<loquax::ui::counters::Counters> {
        let inner = self.lock();
        Some(inner.model.counters()? + inner.store.counters()?)
    }

    /// Run `f` with the pump making no turn (1587), then wake it: a bar
    /// that counts one request reads what that request built, not the
    /// pump's next turn, whose clock events start the model's count afresh
    /// (`loquax::ui::update`) and which a request wakes. The hold is read
    /// under the session's lock, so a turn under way when it is set ends
    /// before `f` runs. For the facade's bars.
    #[doc(hidden)]
    pub fn with_pump_held<T>(&self, f: impl FnOnce(&Self) -> T) -> T {
        use std::sync::atomic::Ordering;
        struct Release<'a>(&'a Session);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.held.store(false, Ordering::SeqCst);
                self.0.wake.notify_one();
            }
        }
        self.held.store(true, Ordering::SeqCst);
        let _release = Release(self);
        // The turn under way, if one is, ends here.
        drop(self.lock());
        f(self)
    }

    /// One pump turn here and now (1595), as the pump's thread makes it:
    /// with the pump held, a bar sees what the turn after a request does.
    /// For the facade's bars.
    #[doc(hidden)]
    pub fn pump_once(&self) -> bool {
        self.lock().pump()
    }

    /// Whether the pump's last turn changed something of its own (1600):
    /// what it folded beside the request it followed. For the facade's
    /// bars.
    #[doc(hidden)]
    pub fn last_turn_was_its_own(&self) -> bool {
        self.lock().turn_own
    }

    /// Whether the session's start is through (1600): the engine said it
    /// is ready, and no pull runs, is owed or left a backlog.
    /// Until then the pump's turns fold the start's own events, and a bar
    /// on one request reads them with it. For the facade's bars.
    #[doc(hidden)]
    pub fn started(&self) -> bool {
        let inner = self.lock();
        inner.model.address.is_some() && !inner.pulling && !inner.pull_owed && !inner.backlog
    }

    /// The model's lists revision (1595), for the facade's bars.
    #[doc(hidden)]
    pub fn lists_revision(&self) -> u64 {
        self.lock().model.lists_revision()
    }

    /// Start the model's and the store's counts afresh. For the facade's
    /// bars.
    #[doc(hidden)]
    pub fn reset_counters(&self) {
        let inner = self.lock();
        inner.model.reset_counters();
        inner.store.reset_counters();
    }

    fn lock(&self) -> parking_lot::MutexGuard<'_, Inner> {
        use std::sync::atomic::Ordering;
        // Counted while it waits, so the pump lets go after the event it
        // is folding (1464). A panic under the lock is caught at the JNI
        // edge; parking_lot does not poison, so what it left is the state
        // the next caller sees, as it was before 1496.
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let folded = self.folded.load(Ordering::SeqCst);
        let inner = self.inner.lock();
        let behind = self.folded.load(Ordering::SeqCst).saturating_sub(folded);
        LOCK_WAITS.with(|waits| {
            let (locks, folds) = waits.get();
            waits.set((locks + 1, folds.saturating_add(behind)));
        });
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        inner
    }

    /// The name of the thread the pump's last turn ran on (1496), for the
    /// facade.
    #[doc(hidden)]
    pub fn pump_thread(&self) -> Option<String> {
        self.lock().pump_thread.clone()
    }

    /// A list screen without waiting for the lock (1464) and without
    /// computing it (1491): the [`Scene`] as the pump or the last request
    /// made it, at the model's newest lists revision but for a change the pump
    /// has not turned over yet, which it tells the screens with
    /// `changed` once it has. Before 1491 a caller who found the lock free
    /// computed both lists on its own thread, the UI thread, and every
    /// lock taken (a map frame's too) made them stale.
    fn listed_now(&self, which: Listed) -> Vec<u8> {
        self.scene_of(which).unwrap_or_default()
    }

    /// How often the session asked the file system for a linked peer's
    /// thumbnail (1496), for the facade's bar.
    #[doc(hidden)]
    pub fn photo_stats(&self) -> u64 {
        self.lock().photo_stats.get()
    }

    /// How often the session made the two lists (1491), for the
    /// facade's idle bar.
    #[doc(hidden)]
    pub fn scene_builds(&self) -> u64 {
        self.scene
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .builds
    }

    /// The scene's `which`, when there is one.
    fn scene_of(&self, which: Listed) -> Option<Vec<u8>> {
        let scene = self
            .scene
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match which {
            Listed::Conversations => scene.conversations.clone(),
            Listed::Archived => scene.archived.clone(),
        }
    }

    /// What the user did. The pump is woken after it: whatever the request
    /// started (a send, a pull, a screen in front) runs on the pump's turn,
    /// and an idle pump would otherwise sleep through it (1176).
    pub fn request(&self, request: schema::Request) -> Result<(), String> {
        let done = self.requested(request);
        // The lists as the request left them (1464), for a UI thread that
        // asks while the pump holds the lock; only when the request moved
        // them (1595), and the pump's turn after it does not make them
        // again for the same change.
        let mut inner = self.lock();
        if inner.dirty || inner.scene_at() != Some(inner.model.lists_revision()) {
            inner.show_scene();
            inner.request_shown = true;
        }
        drop(inner);
        self.wake.notify_one();
        done
    }

    fn requested(&self, request: schema::Request) -> Result<(), String> {
        let intent = match decode(request)? {
            Decoded::Intent(intent) => intent,
            Decoded::Platform(platform) => return self.platform(platform),
        };
        let event = match intent {
            Intent::OpenChat { peer } => {
                let mut inner = self.lock();
                inner.apply(UiEvent::Open { peer });
                inner.apply(UiEvent::Run(PaletteCommand::Fetch));
                return Ok(());
            }
            Intent::Send {
                to: Target::Peer(peer),
                body: Some(body),
            } => UiEvent::Write { peer, body },
            Intent::React {
                to: Target::Peer(peer),
                target: Some(target),
                content: Some(content),
            } => {
                let event = UiEvent::React {
                    peer,
                    target,
                    content: content.clone(),
                };
                let mut inner = self.lock();
                inner.used(&content);
                inner.apply(event);
                return Ok(());
            }
            Intent::Reply {
                to: Target::Peer(peer),
                target,
                body: Some(body),
            } => UiEvent::Reply { peer, target, body },
            Intent::Forward { from, key, to } => UiEvent::Forward { from, key, to },
            Intent::SendAgain { peer, key } => UiEvent::SendAgain { peer, key },
            // Scaled and stripped off the lock: a 12 MP picture takes a
            // quarter of a second on a desktop, more on a phone.
            Intent::SendPhoto { peer, data } => {
                let size = self.lock().model.photo_size;
                let prepared = loquax::photo::prepare(&data, size).map_err(|e| e.to_string())?;
                UiEvent::SendPhoto {
                    peer,
                    format: prepared.format.to_string(),
                    data: prepared.data,
                }
            }
            // A file past what a receiver takes is refused here, in the
            // phone's words, for Java's status line (1221).
            Intent::SendFile {
                peer,
                name,
                data,
                size,
            } => {
                let lang = self.lock().lang;
                let whole = usize::try_from(size).unwrap_or(usize::MAX).max(data.len());
                if let Some(why) = loquax::ui::files::refusal(whole) {
                    return Err(why.resolve(lang));
                }
                UiEvent::SendFile { peer, name, data }
            }
            // Coded off the lock (a minute takes a third of a second on a
            // desktop, more on a phone); too short or too long is refused
            // in the phone's words, for Java's status line.
            Intent::SendVoice { peer, pcm, rate } => {
                let lang = self.lock().lang;
                let ms = match rate {
                    0 => 0,
                    rate => {
                        u32::try_from(pcm.len() as u64 * 1000 / u64::from(rate)).unwrap_or(u32::MAX)
                    }
                };
                if let Some(why) = loquax::ui::voice::refusal(ms) {
                    return Err(why.resolve(lang));
                }
                let data = loquax::voice::encode(&pcm, rate)?;
                UiEvent::SendVoice { peer, data }
            }
            Intent::PlayVoice { peer, key } => UiEvent::PlayVoice { peer, key },
            // A pause and the player's end alike: either way it stops.
            Intent::PauseVoice { peer, key } => {
                let mut inner = self.lock();
                inner.apply(UiEvent::PauseVoice { peer, key });
                inner.apply(UiEvent::VoiceEnded { peer, key });
                return Ok(());
            }
            Intent::Delete {
                messages: Messages::Keys { peer, keys },
                confirmed: true,
            } => UiEvent::Delete { peer, keys },
            Intent::Insert(emoji) => {
                self.lock().used(&emoji);
                return Ok(());
            }
            Intent::CloseChat => UiEvent::Close,
            Intent::Pull => {
                let mut inner = self.lock();
                inner.asked_to_pull();
                inner.apply(UiEvent::Run(PaletteCommand::Fetch));
                return Ok(());
            }
            Intent::Network { kind, changed } => {
                self.lock().apply(UiEvent::Network { kind, changed });
                // The pump looks at the rotations soon and says the line.
                self.wake.notify_one();
                return Ok(());
            }
            Intent::Front(front) => {
                let mut inner = self.lock();
                inner.front = front;
                inner.apply(UiEvent::Front(front));
                return Ok(());
            }
            Intent::Viewport { peer, keys } => UiEvent::Viewport { peer, keys },
            Intent::Name(name) => return self.rename(&name),
            Intent::Profile => UiEvent::Profile { open: true },
            Intent::Back if self.lock().model.profile_open => UiEvent::Profile { open: false },
            Intent::AddContact { lxma, name } => return self.add_contact(&lxma, name.as_deref()),
            // The group verbs (1307) are the model's own.
            Intent::Group(verb) => UiEvent::Group(verb),
            Intent::Rename {
                peer: Target::Peer(peer),
                name: Some(name),
            } => UiEvent::Rename { peer, name },
            Intent::Pin {
                peer: Target::Peer(peer),
                on,
            } => {
                return self.change_state(&peer, |state, _| state.pinned = on.apply(state.pinned));
            }
            Intent::Archive {
                peer: Target::Peer(peer),
                on,
            } => {
                return self
                    .change_state(&peer, |state, _| state.archived = on.apply(state.archived));
            }
            Intent::Block {
                peer: Target::Peer(peer),
                on,
            } => {
                return self
                    .change_state(&peer, |state, _| state.blocked = on.apply(state.blocked));
            }
            Intent::Mute {
                peer: Target::Peer(peer),
                secs,
            } => {
                return self.change_state(&peer, |state, now| {
                    state.muted_until = match secs {
                        0 => None,
                        secs if secs < 0 => Some(loquax::ui::state::MUTED_ALWAYS),
                        secs => Some(now.saturating_add(secs)),
                    };
                });
            }
            Intent::Verify {
                peer: Target::Peer(peer),
                on,
            } => UiEvent::Verify {
                peer,
                verified: on.apply(self.lock().model.verified(&peer)),
            },
            Intent::MarkUnread {
                peer: Target::Peer(peer),
                on,
            } => UiEvent::MarkUnread {
                peer,
                unread: on.apply(false),
            },
            Intent::Forget {
                peer: Target::Peer(peer),
                everywhere,
                confirmed: true,
            } => UiEvent::DeleteConversation { peer, everywhere },
            Intent::Share { peer, location } => UiEvent::SharePosition { peer, location },
            Intent::ReadReceipts { on } => {
                UiEvent::ReadReceipts(on.apply(self.lock().model.reading.on))
            }
            Intent::Draft {
                to: Target::Peer(peer),
                text,
                reply,
            } => UiEvent::Draft { peer, text, reply },
            // The camera's photo goes through the same scaling and
            // stripping as one from the gallery (1270), off the lock.
            Intent::StagePhoto { peer, data } => {
                let size = self.lock().model.photo_size;
                let prepared = loquax::photo::prepare(&data, size).map_err(|e| e.to_string())?;
                UiEvent::StagePhoto {
                    peer,
                    format: prepared.format.to_string(),
                    data: prepared.data,
                }
            }
            Intent::Unstage { peer } => UiEvent::Unstage { peer },
            Intent::Attach { peer } => UiEvent::OpenSheet(compose::Sheet::Attach { peer }),
            Intent::Locate { peer } => UiEvent::OpenSheet(compose::Sheet::Position { peer }),
            Intent::CloseSheet => UiEvent::CloseSheet,
            Intent::OwnFix { location } => {
                // A share judges the fix at the time it came (1273).
                let mut inner = self.lock();
                let now = wall_now(inner.offset);
                inner.apply(UiEvent::Clock(now));
                drop(inner);
                UiEvent::OwnFix(location)
            }
            Intent::ShareFix { peer } => UiEvent::ShareFix { peer },
            Intent::StartShare { peer, span } => UiEvent::StartShare { peer, span },
            Intent::StopShare { peer } => UiEvent::StopShare { peer },
            // The recording bar's buttons (1276) are the recording's
            // input like the gestures.
            Intent::Recording { peer, verb } => UiEvent::Record {
                peer,
                input: Input::Verb(verb),
            },
            // The maps are the renderer's, not the model's (1290): off the
            // session's lock, as the Maps page's queries are.
            Intent::DeleteAllRegions { confirmed: true } => {
                let home = self.lock().home.clone();
                return crate::map::delete_all_regions(&home).map(|_| ());
            }
            other => return Err(format!("not a request of the facade: {other:?}")),
        };
        self.lock().apply(event);
        Ok(())
    }

    /// A request that is the phone's own glue, not the user's intent: the
    /// address book's links, and the microphone's gestures and levels
    /// (1276).
    fn platform(&self, request: Platform) -> Result<(), String> {
        match request {
            Platform::Link { peer, contact } => self.lock().link(peer, &contact),
            Platform::Record { peer, input } => {
                self.lock().apply(UiEvent::Record { peer, input });
                Ok(())
            }
            Platform::Browse(event) => {
                self.lock().apply(event);
                Ok(())
            }
            Platform::Rooms(events) => {
                let mut inner = self.lock();
                for event in events {
                    inner.apply(event);
                }
                Ok(())
            }
            Platform::Unlink { peer } => self.lock().unlink(peer),
            Platform::NotNow { peer } => {
                let mut inner = self.lock();
                inner.book.not_now.insert(peer);
                inner.dirty = true;
                links::save(&inner.home, &inner.book)
            }
            Platform::Relink { rows } => {
                let mut inner = self.lock();
                let linked = inner.relink(&rows)?;
                if linked > 0 {
                    let _ = inner.events.send(Event::Notice {
                        v: VERSION,
                        text: format!("linked {linked} from your contacts"),
                    });
                }
                Ok(())
            }
        }
    }

    /// Change one field of the reader's word on `peer`, the others as the
    /// model holds them; `change` gets the time now in unix seconds.
    fn change_state(
        &self,
        peer: &[u8; 16],
        change: impl FnOnce(&mut loquax::home::wire::PeerState, i64),
    ) -> Result<(), String> {
        let peer = *peer;
        let mut inner = self.lock();
        let mut state = inner.model.peer_state(&peer);
        change(&mut state, wall_now(inner.offset).unix_secs);
        inner.apply(UiEvent::PeerState { peer, state });
        inner.dirty = true;
        Ok(())
    }

    /// What the screen wants to show, encoded.
    pub fn query(&self, query: schema::Query) -> Result<Vec<u8>, String> {
        match query {
            schema::Query::Me => return self.me().map(|me| schema::encode(&me)),
            schema::Query::Profile => return self.profile().map(|p| schema::encode(&p)),
            schema::Query::MailboxPage => return self.mailbox_page().map(|p| schema::encode(&p)),
            schema::Query::LinkDevice => return self.link_device().map(|c| schema::encode(&c)),
            schema::Query::Linked => return self.linked().map(|l| schema::encode(&l)),
            schema::Query::MovePhone => return self.move_phone().map(|c| schema::encode(&c)),
            schema::Query::Moving => return self.moving_state().map(|l| schema::encode(&l)),
            schema::Query::Backup { passphrase } => {
                return self
                    .backup(&passphrase)
                    .map(|backup| schema::encode(&backup))
            }
            // The settings pages' map questions (1207) go to the renderer
            // of the app's directory, not under the session's lock.
            schema::Query::MapRegions {
                delete,
                set_autoload,
            } => {
                let home = self.lock().home.clone();
                return crate::map::regions(&home, delete.as_deref(), set_autoload)
                    .map(|regions| schema::encode(&regions));
            }
            schema::Query::MapTheme { set } => {
                let home = self.lock().home.clone();
                return crate::map::theme(&home, set.as_deref())
                    .map(|theme| schema::encode(&theme));
            }
            schema::Query::MapFling { set } => {
                let home = self.lock().home.clone();
                return crate::map::fling(&home, set).map(|fling| schema::encode(&fling));
            }
            // The Privacy page's switch (1233): the model's, which the
            // config keeps (`UiEffect::Remember`).
            // The Chats page's photo size (1220), kept as the receipts'
            // switch is.
            schema::Query::PhotoSize { set } => {
                let mut inner = self.lock();
                if let Some(word) = set {
                    let size = loquax::ui::photos::PhotoSize::from_word(&word)
                        .ok_or_else(|| format!("{word:?} is not small, phone or original"))?;
                    inner.apply(UiEvent::PhotoSize(size));
                }
                return Ok(schema::encode(&schema::PhotoSizeChoice {
                    v: VERSION,
                    ok: true,
                    size: inner.model.photo_size.word().to_string(),
                }));
            }
            // A photo's pixels (1220): its bytes copied under the lock,
            // decoded and scaled off it.
            schema::Query::Photo { peer, key, edge } => {
                let peer = parse_peer(&peer)?;
                let bytes = self
                    .lock()
                    .picture(peer, schema::model_key(key))
                    .ok_or_else(|| "no such photo".to_string())?;
                let pixels =
                    loquax::photo::fit(&bytes, edge.clamp(1, 4096)).map_err(|e| e.to_string())?;
                return Ok(schema::encode(&schema::PhotoPixels {
                    v: VERSION,
                    ok: true,
                    width: pixels.width,
                    height: pixels.height,
                    rgba: schema::Blob(pixels.rgba),
                    full_width: pixels.full.0,
                    full_height: pixels.full.1,
                }));
            }
            // The staged photo's pixels (1270): its bytes copied under the
            // lock, decoded and scaled off it.
            schema::Query::Staged { peer, edge } => {
                let peer = parse_peer(&peer)?;
                let bytes = self
                    .lock()
                    .model
                    .bar
                    .staged
                    .get(&peer)
                    .map(|staged| staged.data.clone())
                    .ok_or_else(|| "no staged photo".to_string())?;
                let pixels =
                    loquax::photo::fit(&bytes, edge.clamp(1, 4096)).map_err(|e| e.to_string())?;
                return Ok(schema::encode(&schema::PhotoPixels {
                    v: VERSION,
                    ok: true,
                    width: pixels.width,
                    height: pixels.height,
                    rgba: schema::Blob(pixels.rgba),
                    full_width: pixels.full.0,
                    full_height: pixels.full.1,
                }));
            }
            // A file to open or share (1221): its message's fields copied
            // under the lock, written under the app's files off it.
            // A voice message's sound (1222): its bytes copied under the
            // lock, decoded off it.
            schema::Query::Voice { peer, key } => {
                let peer = parse_peer(&peer)?;
                let key = schema::model_key(key);
                let (mode, data) = {
                    let inner = self.lock();
                    inner
                        .model
                        .conversations
                        .iter()
                        .find(|c| c.peer == peer)
                        .and_then(|c| c.messages.iter().find(|m| m.activity == key))
                        .and_then(|m| loquax::ui::voice::voice_parts(&m.fields))
                        .map(|(mode, data)| (mode, data.to_vec()))
                        .ok_or_else(|| "no such voice message".to_string())?
                };
                let pcm = loquax::voice::decode(mode, &data)?;
                // The bubble draws the decoded sound's own track from now
                // on (1276), where the model has none: a received message,
                // or ours from before the recording kept one.
                let mut inner = self.lock();
                if inner.model.voice_track(peer, key).is_none() {
                    inner.apply(UiEvent::VoiceTrack {
                        peer,
                        key,
                        track: LevelTrack::from_pcm(&pcm.samples, pcm.rate),
                    });
                    inner.dirty = true;
                }
                drop(inner);
                return Ok(schema::encode(&schema::VoicePcm {
                    v: VERSION,
                    ok: true,
                    rate: pcm.rate,
                    pcm: schema::Blob(pcm.samples.iter().flat_map(|s| s.to_le_bytes()).collect()),
                }));
            }
            schema::Query::SaveFile { peer, key, index } => {
                let peer = parse_peer(&peer)?;
                let key = schema::model_key(key);
                let (home, fields) = {
                    let inner = self.lock();
                    let fields = inner
                        .model
                        .conversations
                        .iter()
                        .find(|c| c.peer == peer)
                        .and_then(|c| c.messages.iter().find(|m| m.activity == key))
                        .map(|m| m.fields.clone())
                        .ok_or_else(|| "no such message".to_string())?;
                    (inner.home.clone(), fields)
                };
                let index = usize::try_from(index).map_err(|e| e.to_string())?;
                let path = loquax::file_save::save(&home, peer, key, index, &fields)?;
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                return Ok(schema::encode(&schema::SavedFile {
                    v: VERSION,
                    ok: true,
                    path: path.display().to_string(),
                    name,
                }));
            }
            // The Browse tab (1337), with what it hands Java once.
            schema::Query::Browse => {
                let mut inner = self.lock();
                let mut screen = crate::browser::screen(&inner.model, inner.lang, VERSION);
                screen.copy = inner.browse_copy.take().unwrap_or_default();
                if let Some((path, name)) = inner.browse_file.take() {
                    screen.file = path;
                    screen.file_name = name;
                }
                return Ok(schema::encode(&screen));
            }
            // The recording bar (1276): the effects are handed out once.
            schema::Query::Recording { peer } => {
                let peer = parse_peer(&peer)?;
                let mut inner = self.lock();
                let (mine, others) = std::mem::take(&mut inner.recorder)
                    .into_iter()
                    .partition::<Vec<_>, _>(|(of, _)| *of == peer);
                inner.recorder = others;
                let effects = mine
                    .into_iter()
                    .map(|(_, effect)| effect_word(effect).to_string())
                    .collect();
                return Ok(schema::encode(&recording(
                    &inner.model,
                    peer,
                    inner.lang,
                    effects,
                )));
            }
            // The Advanced page's public rooms' hub (1577): the core's
            // rows ([`loquax::ui::settings::hub_rows`]) in the phone's
            // language; an address that is none changes nothing and says so.
            schema::Query::RoomsHub { set } => {
                let mut inner = self.lock();
                let mut error = String::new();
                if set.as_deref().map(str::trim) == Some("more") {
                    // "More…" (1599): every hub in the page's scroll.
                    inner.apply(UiEvent::Rooms(loquax::ui::rooms::RoomsEvent::More));
                } else if let Some(set) = set {
                    let choice = match set.trim() {
                        "" | "none" => Some(loquax::ui::rooms::HubChoice::None),
                        text => match loquax::address::parse(text) {
                            Ok(hub) => Some(loquax::ui::rooms::HubChoice::Hub(hub)),
                            Err(_) => None,
                        },
                    };
                    match choice {
                        Some(choice) => {
                            inner.apply(UiEvent::Rooms(loquax::ui::rooms::RoomsEvent::Hub(choice)))
                        }
                        None => error = Text::key(StrId::RoomsHubBad).resolve(inner.lang),
                    }
                }
                return Ok(schema::encode(&rooms_hub_choice(
                    &inner.model,
                    inner.lang,
                    error,
                )));
            }
            schema::Query::Receipts { set } => {
                let mut inner = self.lock();
                if let Some(on) = set {
                    inner.apply(UiEvent::ReadReceipts(on));
                }
                return Ok(schema::encode(&schema::ReceiptsChoice {
                    v: VERSION,
                    ok: true,
                    on: inner.model.reading.on,
                }));
            }
            _ => {}
        }
        // The list screens' own questions never wait for the pump (1464).
        match &query {
            schema::Query::Conversations { search: None } => {
                return Ok(self.listed_now(Listed::Conversations));
            }
            schema::Query::Archived { search: None } => {
                return Ok(self.listed_now(Listed::Archived));
            }
            schema::Query::Code { text, lang } => {
                return Ok(schema::encode(&read_code(
                    text,
                    Lang::of_tag(lang.as_deref().unwrap_or_default()),
                    |_| String::new(),
                )?));
            }
            _ => {}
        }
        let inner = self.lock();
        Ok(match query {
            schema::Query::Conversations { search } => {
                schema::encode(&inner.conversations(search.as_deref()))
            }
            schema::Query::Messages { peer } => schema::encode(&inner.messages(parse_peer(&peer)?)),
            schema::Query::Bar { peer } => schema::encode(&inner.bar(parse_peer(&peer)?)),
            schema::Query::Shares => schema::encode(&inner.shares()),
            schema::Query::Group => schema::encode(&inner.group_screen()),
            schema::Query::Info { peer, key } => {
                let peer = parse_peer(&peer)?;
                let info =
                    loquax::ui::info::message_info(&inner.model, peer, schema::model_key(key))
                        .ok_or_else(|| "no such message".to_string())?;
                schema::encode(&schema::Info {
                    v: VERSION,
                    rows: info
                        .rows
                        .iter()
                        .map(|row| {
                            let (label, value) = row.resolve(inner.lang);
                            schema::InfoRow { label, value }
                        })
                        .collect(),
                    advanced: info.advanced,
                })
            }
            schema::Query::Run { peer, key } => schema::encode(&run(
                &inner.model,
                parse_peer(&peer)?,
                schema::model_key(key),
            )),
            schema::Query::Recents => schema::encode(&schema::Recents {
                v: VERSION,
                emoji: loquax::emoji::load_recents(&inner.home).0,
            }),
            schema::Query::Suggest { peer, candidates } => {
                let peer = parse_peer(&peer)?;
                schema::encode(&suggest(&inner.model.names, &peer, &candidates))
            }
            schema::Query::Contact { lookup } => schema::encode(&schema::Addresses {
                v: VERSION,
                addresses: inner
                    .book
                    .of_contact(&lookup)
                    .iter()
                    .map(|peer| schema::AddressRow {
                        peer: loquax::address::to_hex(peer),
                        name: inner.model.names.label(peer, inner.lang),
                        announced: inner.model.names.announced(peer).map(str::to_string),
                    })
                    .collect(),
            }),
            schema::Query::Detail { peer } => schema::encode(&inner.detail(parse_peer(&peer)?)?),
            schema::Query::Archived { search } => {
                let order = inner.model.archived_order();
                schema::encode(&inner.listed(order, search.as_deref()))
            }
            schema::Query::Blocked => schema::encode(&inner.blocked()),
            schema::Query::People { search } => {
                schema::encode(&inner.listed(inner.model.people(), search.as_deref()))
            }
            schema::Query::Friends { lat_e6, lon_e6 } => schema::encode(&crate::friends::friends(
                &inner.model,
                wall_now(inner.offset).unix_secs,
                lat_e6.zip(lon_e6),
                inner.lang,
                |peer| inner.photo(peer),
            )),
            schema::Query::Shared => schema::encode(&inner.shared()?),
            schema::Query::Lxma { text, lang } => schema::encode(&read_code(
                &text,
                lang.as_deref().map_or(inner.lang, Lang::of_tag),
                |address| {
                    inner
                        .model
                        .names
                        .shown(address)
                        .unwrap_or_default()
                        .to_string()
                },
            )?),
            schema::Query::Me
            | schema::Query::Profile
            | schema::Query::MailboxPage
            | schema::Query::LinkDevice
            | schema::Query::Linked
            | schema::Query::MovePhone
            | schema::Query::Moving
            | schema::Query::Backup { .. }
            | schema::Query::MapRegions { .. }
            | schema::Query::MapTheme { .. }
            | schema::Query::MapFling { .. }
            | schema::Query::Receipts { .. }
            | schema::Query::RoomsHub { .. }
            | schema::Query::PhotoSize { .. }
            | schema::Query::Photo { .. }
            | schema::Query::Staged { .. }
            | schema::Query::SaveFile { .. }
            | schema::Query::Recording { .. }
            | schema::Query::Browse
            | schema::Query::Code { .. }
            | schema::Query::Voice { .. } => return Err("answered above".to_string()),
            // The viewer's zoom needs no session: `lib.rs` answers it.
            schema::Query::Zoom(ask) => return Ok(crate::viewer::answer(&ask)),
            // The Rooms tab (1384).
            schema::Query::Rooms => crate::rooms::encode(&inner.model, inner.lang, VERSION),
            // The Explore tab (1578).
            schema::Query::Explore => crate::explore::encode(&inner.model, inner.lang, VERSION),
            // The settings' sections (1450).
            schema::Query::Settings => schema::encode(&settings_list(inner.lang)),
            schema::Query::Links => schema::encode(&schema::Links {
                v: VERSION,
                links: inner
                    .book
                    .links
                    .iter()
                    .map(|(peer, link)| schema::LinkRow {
                        peer: loquax::address::to_hex(peer),
                        lookup: link.lookup.clone(),
                        contact_id: link.contact_id,
                        name: link.name.clone(),
                    })
                    .collect(),
            }),
        })
    }

    /// The map's pins as the session's model knows them now.
    pub fn map_pins(&self) -> Vec<MapPin> {
        self.lock().model.map_pins()
    }

    /// The peers with a position inside the tracks' time window (1386):
    /// what "fit all" brings into view; none before the clock is known.
    pub fn map_inside(&self) -> Vec<[u8; 16]> {
        let inner = self.lock();
        match inner.model.now {
            Some(now) => loquax::ui::tracks::inside(
                &inner.model.track_input(),
                inner.model.track_window,
                &now,
            ),
            None => Vec::new(),
        }
    }

    /// The map's tracks on a view whose screen pixel spans `unit` world
    /// units, drawn at `scale` (1385), after a window `chosen` in the map
    /// menu's sheet (1419); the window they are drawn in, and the phone's
    /// language the map's title and sheet are said in.
    pub fn map_tracks(
        &self,
        unit: f64,
        scale: f32,
        chosen: Option<loquax::ui::tracks::TrackWindow>,
    ) -> (
        Vec<loquax::ui::tracks::Track>,
        loquax::ui::tracks::TrackWindow,
        Lang,
    ) {
        let mut inner = self.lock();
        if let Some(chosen) = chosen {
            inner.model.track_window = chosen;
        }
        let window = inner.model.track_window;
        (inner.model.map_tracks(unit, scale), window, inner.lang)
    }

    /// The next event, waiting at most `wait`.
    pub fn next_event(&self, wait: Duration) -> Option<Event> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match events.recv_timeout(wait) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Event::Stopped {
                v: VERSION,
                detail: String::new(),
            }),
        }
    }

    /// Stop the node and the runtime. Marks still running get a moment.
    pub fn stop(&self) {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(runtime) = runtime else {
            return;
        };
        {
            let mut inner = self.lock();
            // A draft typing has not paused on yet is kept all the same
            // (1219).
            inner.model.flush_drafts();
            inner.persist();
            inner.gone = true;
            let _ = inner.events.send(Event::Stopped {
                v: VERSION,
                detail: String::new(),
            });
        }
        // The pump sees the session gone on its next turn, which this
        // wakes, and ends before the runtime its timers run on goes.
        self.wake.notify_one();
        let pump = self
            .pump
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(pump) = pump {
            if pump.thread().id() != std::thread::current().id() && pump.join().is_err() {
                tracing::warn!("loquax: the pump's thread panicked");
            }
        }
        // The mailbox closes its node before the runtime goes, so its lock
        // and its store are let go of in order.
        let here = self.here().take();
        let finish = move || {
            if let Some(mut here) = here {
                runtime.block_on(here.stop());
            }
            runtime.shutdown_timeout(Duration::from_secs(2));
        };
        // On a runtime's own thread, a test's async body dropping the
        // session as an assertion unwinds, neither `block_on` nor the
        // shutdown's wait may block: tokio panics, and a panic in a Drop
        // during unwinding aborts the process (1274). A thread of its own
        // may block, so the stop is done there and waited for.
        if tokio::runtime::Handle::try_current().is_ok() {
            if std::thread::spawn(finish).join().is_err() {
                tracing::warn!("loquax: the session's stop panicked on its own thread");
            }
        } else {
            finish();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Inner {
    #[allow(clippy::too_many_arguments)]
    fn new(
        home: &Path,
        address: [u8; 16],
        attached: Attached,
        client: HomeClient,
        offset: i32,
        lang: Lang,
        camera: bool,
        events: Sender<Event>,
        runtime: tokio::runtime::Handle,
        watch: tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
        wake: Arc<tokio::sync::Notify>,
    ) -> Result<Self, String> {
        let store = Store::open_in(home, address).map_err(|error| error.to_string())?;
        let config = loquax::config::load(home).map_err(|error| error.to_string())?;
        let mut model = known(home, &store)?;
        model.lang = lang;
        model.home_client = true;
        // A phone that never settled a pull, a new one or one the mailbox
        // moved to, is about to be handed the home's history (1429): none
        // of it is read by arriving.
        model.history = loquax::home::client::history_owed(home);
        model.list_apart = true;
        // The phone hosts lnomad (1337), opened from Explore: on unless
        // the config's `browse_tab` says off (1618: the walk found every
        // page listed and none openable).
        model.browse.enabled = config.browse_on();
        // The Rooms tab is there where the config names a hub (1384), as
        // on the terminal and the window.
        model.rooms.hub = config.rooms_hub;
        model.rooms.enabled = config.rooms_hub.is_some();
        model.rooms.told = config.rooms_told;
        // The Explore tab's order (1578), per device.
        model.explore.az = config.explore_az.unwrap_or(false);
        // What the phone heard before (1632): its pages, hubs and the
        // suggestion, each with its old last seen.
        match loquax::heard::load(&store) {
            Ok((pages, hubs)) => {
                loquax::ui::explore::restore_heard(&mut model, pages, hubs, config.rooms_suggested)
            }
            Err(error) => tracing::warn!("HEARD not read: {error}"),
        }
        model.reading.on = config.read_receipts.unwrap_or(true);
        model.photo_size = config.photo_size.unwrap_or_default();
        model.bar.abilities = compose::Abilities::phone(camera);
        for (message_id, source, packed) in store.unverified().map_err(|e| e.to_string())? {
            let verify = Command::Verify {
                message_id,
                source,
                packed,
            };
            if attached.outbox.submit(verify).is_err() {
                break;
            }
        }
        let now = wall_now(offset);
        update(&mut model, UiEvent::Clock(now));
        model.restore(store.restore().map_err(|error| error.to_string())?);
        // What the propagation node is still to be asked about (1409).
        model.pn_asker =
            loquax::pn_status::Asker::restore(store.pn_asks().map_err(|error| error.to_string())?);
        // The petnames the home had not taken when the session ended
        // (1635): handed again, and the reader's word over the first pull.
        model.restore_owed_petnames(store.owed_petnames().map_err(|error| error.to_string())?);
        // The peer states likewise (1640).
        model.restore_owed_peer_states(
            store
                .owed_peer_states()
                .map_err(|error| error.to_string())?,
        );
        // A share runs on across a restart (1273); one whose time ran out
        // meanwhile ends at the next clock with its end report.
        model.restore_shares(store.shares().map_err(|error| error.to_string())?);
        // The phone starts on the list, with nothing open: the model's
        // terminal habit of opening the first conversation would read what
        // arrives there before anyone looked.
        update(&mut model, UiEvent::Close);
        model.activity = model
            .activity
            .max(store.last_key().map_err(|error| error.to_string())?);
        // The pull schedule of the terminal: while the app runs.
        model.sync = Some(loquax::sync::Schedule::new(
            config
                .sync_interval
                .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
        ));
        let (finished_tx, finished_rx) = channel();
        Ok(Self {
            model,
            store,
            attached,
            fanout: Fanout::default(),
            client,
            home: home.to_path_buf(),
            offset,
            lang,
            sends: VecDeque::new(),
            browse_owed: Vec::new(),
            fetching: None,
            browse_copy: None,
            browse_file: None,
            rooming: None,
            sending: false,
            pull_owed: false,
            pulling: false,
            pages_asking: None,
            pull_asked: false,
            pull_line: PullLine::default(),
            offline: Offline::default(),
            finished_tx,
            finished_rx,
            events,
            runtime,
            origin: std::time::Instant::now(),
            dirty: true,
            backlog: false,
            waiting: Arc::default(),
            folded: Arc::default(),
            scene: Arc::default(),
            counts: 0,
            gone: false,
            link: None,
            // The first pump looks, and says what it saw.
            link_looked: None,
            link_since: None,
            last_contact: None,
            about: Arc::default(),
            link_owed: false,
            wake,
            watch,
            watching: false,
            front: false,
            fallback: Fallback::default(),
            book: began(home),
            photos: std::cell::RefCell::default(),
            photo_stats: std::cell::Cell::new(0),
            row_cache: std::cell::RefCell::default(),
            request_shown: false,
            turn_own: false,
            pump_thread: None,
            just_added: std::collections::BTreeSet::new(),
            keys_told: Arc::default(),
            recorder: Vec::new(),
            network: loquax::home::rendezvous::Network::new(),
            reconnecting: false,
            redials_seen: 0,
            costs: DeviceCosts::default(),
        })
    }

    /// One turn of the pump: what the engine said, what the runners
    /// finished, the clock. `false` once the session is over.
    /// The Browse tab's turn (1337 pass two): what the fetch brought goes
    /// to the hosted browser, and what the browser asked of the platform
    /// goes to the fetch, the clipboard or the downloads.
    fn browse(&mut self) {
        while let Some(told) = self
            .fetching
            .as_mut()
            .and_then(|fetching| fetching.told.try_recv().ok())
        {
            match told {
                loquax::browse_fetch::Told::Browse(event) => self.apply(UiEvent::Browse(event)),
                loquax::browse_fetch::Told::File { name, bytes } => {
                    let event = self.keep_download(&name, &bytes);
                    self.apply(UiEvent::Browse(event));
                }
                // The phone asks for no pictures yet (below).
                loquax::browse_fetch::Told::Picture { .. } => {}
            }
        }
        for effect in std::mem::take(&mut self.browse_owed) {
            let ask = match effect {
                lnomad::Effect::Navigate(target) => loquax::browse_fetch::Ask::Navigate(target),
                lnomad::Effect::Download(target) => loquax::browse_fetch::Ask::Download(target),
                lnomad::Effect::SetIdentify { dest, on } => {
                    loquax::browse_fetch::Ask::Identify { dest, on }
                }
                lnomad::Effect::Cancel => loquax::browse_fetch::Ask::Cancel,
                // The browser said "copied" already; Java writes it.
                lnomad::Effect::Copy(text) => {
                    self.browse_copy = Some(text);
                    self.dirty = true;
                    continue;
                }
                // Not on the phone yet: a page's pictures, an external
                // link, and the bookmarks and pins kept past the session.
                lnomad::Effect::FetchImages(_)
                | lnomad::Effect::SaveImage { .. }
                | lnomad::Effect::OpenImage { .. }
                | lnomad::Effect::OpenExternal(_)
                | lnomad::Effect::SaveBookmarks
                | lnomad::Effect::SavePins
                | lnomad::Effect::Write(_)
                | lnomad::Effect::Quit => continue,
            };
            if let Some(fetching) = &self.fetching {
                // A fetch that ended has nothing more to bring either.
                let _ = fetching.asks.send(ask);
            }
        }
    }

    /// The Rooms' turn (1384): what the hub link said goes to the model.
    fn rooms(&mut self) {
        while let Some(told) = self
            .rooming
            .as_mut()
            .and_then(|rooming| rooming.told.try_recv().ok())
        {
            self.apply(UiEvent::Rooms(told));
        }
    }

    /// A downloaded file kept under the app's files, as a bubble's file is
    /// (`loquax::file_save`), for Java to open or share; what the browser
    /// is told of it.
    fn keep_download(&mut self, name: &str, bytes: &[u8]) -> BrowseEvent {
        let dir = self.home.join(loquax::file_save::DIR).join(DOWNLOADS);
        match lnomad::download::save_to_dir(&dir, name, bytes) {
            Ok(path) => {
                let kept = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.to_string());
                self.browse_file = Some((path.display().to_string(), kept.clone()));
                self.dirty = true;
                BrowseEvent::Downloaded {
                    name: kept,
                    bytes: bytes.len(),
                    // The reader sees the app, not its private folder.
                    dir: "Loquax".to_string(),
                }
            }
            Err(error) => BrowseEvent::DownloadFailed(lnomad::app::strings::Text::Raw(format!(
                "{name}: {error}"
            ))),
        }
    }

    /// Fold the engine's events the fan-out took into the screen and the
    /// store.
    fn fold(&mut self, events: Vec<OutboxEvent>) {
        for event in events {
            if runner::already_held(&self.store, &event) {
                continue;
            }
            runner::keep_signature(&mut self.store, &event);
            let named = match &event {
                OutboxEvent::Announced {
                    destination,
                    name: Some(_),
                    ..
                } => Some(*destination),
                _ => None,
            };
            self.apply(UiEvent::Engine(event));
            if let Some(peer) = named {
                runner::keep_name(&mut self.store, &self.model.names, peer);
            }
        }
    }

    fn pump(&mut self) -> bool {
        if self.gone {
            return false;
        }
        self.costs.window.pumps += 1;
        // A request's own change is in the lists it made (1595): this
        // turn's dirt starts afresh, and the `Changed` the change owes the
        // screens is still said at the turn's end.
        let shown = std::mem::take(&mut self.request_shown);
        let owed = shown && std::mem::take(&mut self.dirty);
        // The engine's events a slice at a time (1446): each one folded is
        // a store commit, and a first pull of a big mailbox brings
        // thousands. The pump holds the session's lock for its whole turn,
        // and the screens wait on that lock, so the turn stops after
        // [`SLICE`] and the next one comes after [`BACKLOG_PUMP`], in which
        // a waiting caller takes the lock. A caller waiting for the lock
        // ends the turn after the event in hand (1464): a screen's command
        // waits for one commit, not for a slice.
        // A caller that waits already when the turn begins goes first
        // (1610): it was woken when the last turn let go, and a starved
        // thread can take longer than [`BACKLOG_PUMP`] to run. Folding the
        // first event regardless, every turn it missed would be one more
        // commit it waits behind; this way it waits behind one at most.
        let mut slicing = loquax::fanout::Slicing::new(SLICE);
        let yielded = self.waiting.load(std::sync::atomic::Ordering::SeqCst) > 0;
        if !yielded {
            loop {
                let waited = self.waiting.load(std::sync::atomic::Ordering::SeqCst) > 0;
                match slicing.next(&mut self.fanout, &mut self.attached.outbox, waited) {
                    Ok(Some(events)) => {
                        self.folded.fetch_add(
                            u64::try_from(events.len()).unwrap_or(u64::MAX),
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        self.fold(events);
                    }
                    Ok(None) => break,
                    Err(gone) => {
                        self.gone = true;
                        let _ = self.events.send(Event::Stopped {
                            v: VERSION,
                            detail: gone.to_string(),
                        });
                        return false;
                    }
                }
            }
        }
        self.backlog = yielded || slicing.backlog();
        while let Ok(finished) = self.finished_rx.try_recv() {
            match finished {
                Finished::Send(event) => {
                    self.sending = false;
                    self.apply(event);
                }
                Finished::Pull(end, stored) => self.pulled(end, stored),
                Finished::Event(event) => self.apply(event),
            }
        }
        self.browse();
        self.rooms();
        // The network first: a re-dial is said before the link it dropped
        // and brought back (1346).
        self.look_at_network();
        self.look_at_link();
        let elapsed = self.elapsed_ms();
        if let Some(say) = self.pull_line.tick(elapsed) {
            self.say(say);
        }
        self.watched(elapsed);
        // The bar under the list's top bar (1448, walk finding 14).
        self.dirty |= self.offline.look(self.watching, elapsed);
        // Explore says how old the mailbox's list is (1644), on the same
        // word as the bar: a start's moment without a link says nothing.
        let up = !self.offline.offline();
        if self.model.explore.mailbox.link != Some(up) {
            self.apply(UiEvent::Explore(
                loquax::ui::explore::ExploreEvent::Mailbox(up),
            ));
        }
        // Applied, not quiet: a running share sends from the clock (1273).
        self.apply(UiEvent::Clock(wall_now(self.offset)));
        self.quiet(UiEvent::Tick);
        self.apply(UiEvent::Elapsed(elapsed));
        let dirty = std::mem::take(&mut self.dirty);
        self.turn_own = dirty;
        // The lists again only when what they show moved (1491): an idle
        // turn, the clock's three events folded, makes none, nor does a
        // node the Browse tab heard of, nor the request's own change its
        // lists already show (1595).
        if dirty || self.scene_at() != Some(self.model.lists_revision()) {
            self.show_scene();
        }
        if dirty || owed {
            // The lists first: a screen told `changed` reads them at once,
            // and may find the lock held by the next turn (1464).
            let _ = self.events.send(Event::Changed {
                v: VERSION,
                counts: self.counts,
            });
        }
        true
    }

    /// How long the pump may sleep before its next turn (1176): [`PUMP`]
    /// while anything is going on, else until the pull schedule's next
    /// pull, at most [`IDLE_PUMP`].
    fn next_pump(&self) -> Duration {
        // Events still wait in the engine's queue (1446): the next slice
        // soon, after a screen waiting on the lock had its turn.
        if self.backlog {
            return BACKLOG_PUMP;
        }
        let busy = self.front
            || !self.watching
            || self.sending
            || self.pulling
            || self.pull_owed
            || !self.sends.is_empty()
            || self.link.is_none()
            || self.link_owed
            || self.reconnecting
            || self.network.reconnecting()
            || self.dirty;
        if busy {
            return PUMP;
        }
        let now = self.elapsed_ms();
        let sync = self.model.sync.as_ref();
        if sync.and_then(|sync| sync.due(now)).is_some() {
            return PUMP;
        }
        let until_sync = sync
            .and_then(|sync| sync.next_ms())
            .map(|next| Duration::from_millis(next.saturating_sub(now)));
        // A running share's cadence and end are on the clock (1273).
        match self.model.shares.running.is_empty() {
            true => idle_wait(until_sync),
            false => idle_wait(until_sync).min(SHARE_PUMP),
        }
    }

    /// What the watching link said: a wake past the cursor pulls, as does
    /// a minute in front with the link down. The pull is the one a send
    /// starts, behind a running one; its messages notify as any pull's do.
    fn watched(&mut self, elapsed: u64) {
        let mut wake = false;
        while let Ok(event) = self.watch.try_recv() {
            match event {
                WatchEvent::Watching(up) => {
                    // The list's status dot (1199) shows it: a `changed`.
                    self.dirty |= self.watching != up;
                    // The mailbox page's "connected for" (1647): from
                    // the watching link's change, and the mailbox asked
                    // again for its word on each new link, which may be
                    // to a mailbox that restarted.
                    if self.watching != up || self.link_since.is_none() {
                        let wall = wall_now(self.offset).unix_secs;
                        self.link_since = Some(wall);
                        if up {
                            self.last_contact = Some(wall);
                            self.ask_about(0);
                        }
                    }
                    self.watching = up;
                }
                WatchEvent::Wake(newest) => {
                    let cursor = loquax::home::client::load_cursor(&self.home);
                    wake |= watch::wants_pull(newest, cursor);
                }
            }
        }
        if self.fallback.due(elapsed, self.watching, self.front) {
            tracing::info!("SYNC the watching link is down; pulling");
            wake = true;
        }
        if wake {
            self.apply(UiEvent::HomeWake);
        }
    }

    /// Once a second while the pump runs fast, on every pump while it
    /// idles: whether the TCP interface is up, told when it changed and on
    /// the first look.
    fn look_at_link(&mut self) {
        let now = self.elapsed_ms();
        // A look skipped here is owed (1346): the pump that a carrier change
        // woke inside the second may not go back to its idle sleep before
        // it looked, or the change is said only at the idle pump's turn.
        self.link_owed = self
            .link_looked
            .is_some_and(|at| now.saturating_sub(at) < LINK_EVERY_MS);
        if self.link_owed {
            return;
        }
        self.link_looked = Some(now);
        self.costs.read(&self.client.node);
        if let Some(line) = self.costs.summary(now) {
            tracing::info!("{line}");
        }
        // Up only once the re-dial is over: "back" comes before the link,
        // whichever of the two the node saw first, and a link held back
        // for it is looked at again.
        let carried = link_up(&self.client.node);
        let up = carried && !self.reconnecting;
        self.link_owed |= carried && self.reconnecting;
        if up && self.link == Some(false) {
            self.ask_for_home_path();
        }
        if self.watching {
            self.last_contact = Some(wall_now(self.offset).unix_secs);
        }
        if self.link != Some(up) {
            self.link = Some(up);
            let _ = self.events.send(Event::Link { v: VERSION, up });
            // The mailbox's own word, once per connection (1642).
            if up {
                self.ask_about(0);
            }
        }
    }

    /// Ask the mailbox what it says of itself (1642, `/about`), unless an
    /// ask runs or one began less than `fresh_ms` ago. The answer lands in
    /// [`Inner::about`]; the mailbox page reads it on its next look.
    fn ask_about(&mut self, fresh_ms: u64) {
        let now = self.elapsed_ms();
        {
            let mut about = lock_about(&self.about);
            if about.asking
                || about
                    .began
                    .is_some_and(|at| fresh_ms > 0 && now.saturating_sub(at) < fresh_ms)
            {
                return;
            }
            about.asking = true;
            about.began = Some(now);
        }
        let node = self.client.node.clone();
        let public_key = self.client.public_key;
        let device = self.client.device.clone();
        let state = Arc::clone(&self.about);
        self.runtime.spawn(async move {
            use loquax::home::ask::HomeError;
            use loquax::ui::mailbox_page::Told;
            let told = match loquax::home::client::about(&node, &public_key, &device, BUDGET).await
            {
                Ok(about) => {
                    // The commits stay here, never on the page (1647).
                    tracing::info!(
                        "HOME about host={:?} version={:?} ours={:?} started={:?} devices={}",
                        about.host,
                        about.version,
                        loquax::VERSION,
                        about.started,
                        about.devices.len()
                    );
                    Some(Told::Told(about))
                }
                Err(HomeError::Unsupported(_)) => {
                    tracing::info!("HOME about unsupported: the mailbox is older");
                    Some(Told::Older)
                }
                Err(error) => {
                    tracing::warn!("HOME about: {error}");
                    None
                }
            };
            let mut about = lock_about(&state);
            about.asking = false;
            if let Some(told) = told {
                about.told = told;
            }
        });
    }

    /// After a network change (1298): the line says "Reconnecting…" while
    /// a rotation that dropped its connection for it is not up again, and
    /// is gone once every one is. A re-dial that began and is up again
    /// since the last look says both, in order (1334): the pump looks on
    /// each begin and end, but a loopback or fast entry is up before it.
    fn look_at_network(&mut self) {
        // A re-dial that came and went between two looks is said all the
        // same: "reconnecting", then "back" in the same turn (1346).
        let redials = self.network.redials();
        if redials != self.redials_seen {
            self.redials_seen = redials;
            if !self.reconnecting {
                self.say_network(true);
            }
            // The re-dial dropped the connection whether a look saw it
            // down or not: said so, and the next look says it up again.
            if self.link == Some(true) {
                self.link = Some(false);
                let _ = self.events.send(Event::Link {
                    v: VERSION,
                    up: false,
                });
            }
            self.link_owed = true;
        }
        let now = self.network.reconnecting();
        if now != self.reconnecting {
            self.say_network(now);
        }
    }

    fn say_network(&mut self, reconnecting: bool) {
        self.reconnecting = reconnecting;
        let state = match reconnecting {
            true => "reconnecting",
            false => "back",
        };
        tracing::info!("NETWORK {state}");
        let _ = self.events.send(Event::Network {
            v: VERSION,
            state: state.to_string(),
        });
    }

    /// The interface came back: ask for the home destination's path. The
    /// home node never announces that destination, so a transport node that
    /// restarted (the box rebooted) knows no path to it until someone asks,
    /// while this leaf still holds the path from before and would send its
    /// link requests into the void until they time out. The path request
    /// makes the transport ask the home node, which answers it.
    fn ask_for_home_path(&self) {
        let Ok(hash) = loquax::home::destination_hash(&self.client.public_key) else {
            return;
        };
        let node = std::sync::Arc::clone(&self.client.node);
        self.runtime.spawn(async move {
            let destination = leviculum_core::DestinationHash::new(hash);
            if let Err(error) = node.request_path(&destination).await {
                tracing::info!("no path request to the home node: {error}");
            }
        });
    }

    /// Fold an event that changes nothing the screen shows by itself.
    fn quiet(&mut self, event: UiEvent) {
        let _ = update(&mut self.model, event);
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// The user asked for a pull (Refresh): the next one is theirs, or the
    /// running one when one runs.
    fn asked_to_pull(&mut self) {
        match self.pulling {
            true => {
                if let Some(say) = self.pull_line.asked() {
                    self.say(say);
                }
            }
            false => self.pull_asked = true,
        }
    }

    /// Tell the status line about a pull. Not while the list says "no
    /// connection" (1448): "looking for messages" and its failure beside
    /// it only say again that nothing gets through.
    fn say(&self, say: Say) {
        if self.offline.offline() && !matches!(say, Say::Done(new) if new > 0) {
            return;
        }
        let (state, detail) = match say {
            Say::Running => ("running", String::new()),
            Say::Done(new) => ("done", new.to_string()),
            Say::Failed(error) => ("failed", error),
        };
        let _ = self.events.send(Event::Sync {
            v: VERSION,
            state: state.to_string(),
            detail,
        });
    }

    /// Fold one event, keep what it changed, run what it asks for.
    fn apply(&mut self, event: UiEvent) {
        // What a fetch starts and ends with goes to the line as a `sync`
        // event ([`PullLine`]), so the terminal's toasts for it ("fetching
        // the mailbox", "no new messages") would only say it twice.
        let pull_words = matches!(
            event,
            UiEvent::FetchDone(_) | UiEvent::Run(PaletteCommand::Fetch)
        );
        let shown = redraws(&event);
        let toasts = self.model.toasts.len();
        let effects = update(&mut self.model, event);
        for toast in self
            .model
            .toasts
            .iter()
            .skip(toasts)
            .filter(|_| !pull_words)
        {
            let _ = self.events.send(Event::Notice {
                v: VERSION,
                text: toast.text.clone(),
            });
        }
        self.dirty |= shown || !effects.is_empty();
        self.persist();
        let mut answers = Vec::new();
        for effect in effects {
            match effect {
                // The store's answer at once (1496), as the terminal's shell
                // gives it: the older page the viewport reached, a
                // restart's deferred newest page, a stored search. Before,
                // this fell into the catch-all and the phone's chat ended
                // after its first page.
                UiEffect::Store(read) => answers.push(self.read(read)),
                UiEffect::Send { to, body, fields } => self.sends.push_back((to, body, fields)),
                UiEffect::Fetch => self.pull_owed = true,
                // Explore opened (1644): the mailbox's list asked again.
                UiEffect::AskPages => self.ask_pages(),
                UiEffect::Browse(effect) => self.browse_owed.push(effect),
                // The Rooms (1384): the hub link does it; what the hub says
                // comes back on the pump.
                UiEffect::Rooms(ask) => {
                    if let Some(rooming) = &self.rooming {
                        let _ = rooming.asks.send(ask);
                    }
                }
                // Every rotation of the session hears it (1298).
                UiEffect::Network { kind, changed } => self.network.say(kind, changed),
                UiEffect::Log(line) | UiEffect::Notice(line) | UiEffect::PnLine(line) => {
                    tracing::info!("{line}")
                }
                // A client of its home asks no propagation node (1409): the
                // mailbox asks, its own or the app's, and the answer comes
                // as a pulled entry.
                UiEffect::AskPn { .. } => {}
                // A petname set here or pulled (1139): the contacts file
                // in the app's directory, which the next start reads.
                UiEffect::Contact(contact) => {
                    if let Err(error) = loquax::names::save_contact(&self.home, &contact) {
                        let _ = self.events.send(Event::Notice {
                            v: VERSION,
                            text: format!("name not saved: {error}"),
                        });
                    }
                    self.dirty = true;
                }
                // A setting changed on a page (1233): the config in the
                // app's directory, which the next start reads; the
                // screens draw the marks again.
                // The public rooms' hub the Advanced page chose (1577).
                UiEffect::RememberHash { key, hash } => {
                    if let Err(error) = loquax::config::save_hash(&self.home, key, hash) {
                        let _ = self.events.send(Event::Notice {
                            v: VERSION,
                            text: format!("{key} not saved: {error}"),
                        });
                    }
                    self.dirty = true;
                }
                UiEffect::Remember { key, value } => {
                    if let Err(error) = loquax::config::save(&self.home, key, value) {
                        let _ = self.events.send(Event::Notice {
                            v: VERSION,
                            text: format!("{key} not saved: {error}"),
                        });
                    }
                    self.dirty = true;
                }
                // The recorder's work (1276), for Java's next
                // `Query::Recording`.
                UiEffect::Recording { peer, effect } => self.recorder.push((peer, effect)),
                // A group (1307): the home makes it, through the runner the
                // terminal runs.
                UiEffect::CreateGroup { name, members } => {
                    let client = self.client.clone();
                    let done = self.finished_tx.clone();
                    let wake = Arc::clone(&self.wake);
                    self.runtime.spawn(async move {
                        let event = runner::create_group(client, name, members, BUDGET).await;
                        let _ = done.send(Finished::Event(event));
                        wake.notify_one();
                    });
                }
                UiEffect::AddMembers { group, members } => {
                    let client = self.client.clone();
                    let done = self.finished_tx.clone();
                    let wake = Arc::clone(&self.wake);
                    self.runtime.spawn(async move {
                        let event = runner::add_members(client, group, members, BUDGET).await;
                        let _ = done.send(Finished::Event(event));
                        wake.notify_one();
                    });
                }
                UiEffect::DeleteGroup { group } => {
                    let client = self.client.clone();
                    let done = self.finished_tx.clone();
                    let wake = Arc::clone(&self.wake);
                    self.runtime.spawn(async move {
                        let event = runner::delete_group(client, group, BUDGET).await;
                        let _ = done.send(Finished::Event(event));
                        wake.notify_one();
                    });
                }
                // What the terminal does with its screen and its maps has
                // no place here yet.
                _ => {}
            }
        }
        for answer in answers {
            self.apply(answer);
        }
        self.start_next();
    }

    /// What the screen asked the store, answered as the event the model
    /// waits for; a store that fails says so as a notice.
    fn read(&mut self, read: StoreRead) -> UiEvent {
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
        answer.unwrap_or_else(|error| UiEvent::Notice(format!("history: {error}")))
    }

    /// The fold's store writes, and its marks to the home node.
    fn persist(&mut self) {
        let writes = self.model.take_writes();
        if let Err(error) = self.store.write(&writes) {
            let _ = self.events.send(Event::Notice {
                v: VERSION,
                text: format!("not saved: {error}"),
            });
        }
        // What the phone heard (1632, walk 4 new 15: a restart forgot
        // every page): the next start lists it again.
        let heard = loquax::ui::explore::take_heard(&mut self.model);
        if heard != loquax::ui::explore::HeardOwed::default() {
            if let Err(error) = loquax::heard::keep(&mut self.store, &self.home, heard) {
                tracing::warn!("HEARD not kept: {error}");
            }
        }
        // The petnames owed to the home (1635): what it took comes back
        // as an event, the rest goes with the next hand.
        let petnames = self.model.take_petnames();
        if !petnames.is_empty() {
            let client = self.client.clone();
            let done = self.finished_tx.clone();
            let wake = Arc::clone(&self.wake);
            self.runtime.spawn(async move {
                let event = runner::hand_petnames(client, petnames, BUDGET).await;
                let _ = done.send(Finished::Event(event));
                wake.notify_one();
            });
        }
        // The Browse tab's bookmark changes (1579): the phone keeps none
        // past the session, its home keeps them for every device.
        let bookmarks = std::mem::take(&mut self.model.browse.outgoing);
        if !bookmarks.is_empty() {
            self.runtime.spawn(runner::hand_bookmarks(
                self.client.clone(),
                bookmarks,
                BUDGET,
            ));
        }
        // The peer states owed to the home (1640), as the petnames.
        let states = self.model.take_peer_states();
        if !states.is_empty() {
            let client = self.client.clone();
            let done = self.finished_tx.clone();
            let wake = Arc::clone(&self.wake);
            self.runtime.spawn(async move {
                let event = runner::hand_peer_states(client, states, BUDGET).await;
                let _ = done.send(Finished::Event(event));
                wake.notify_one();
            });
        }
        let marks = self.model.take_marks();
        if marks.is_empty() {
            return;
        }
        self.runtime
            .spawn(runner::hand_marks(self.client.clone(), marks, BUDGET));
    }

    /// Link `peer` to the contact `contact` describes, or refresh the
    /// link's copy: the table, the photo, the name every screen shows.
    fn link(&mut self, peer: [u8; 16], contact: &schema::ContactCopy) -> Result<(), String> {
        let name = loquax::names::sanitise(&contact.name)
            .ok_or_else(|| "the contact has no name to show".to_string())?;
        if contact.lookup.trim().is_empty() || contact.lookup.contains('\n') {
            return Err("the contact has no lookup key".to_string());
        }
        self.photos.get_mut().remove(&peer);
        links::keep_photo(
            &self.home,
            &peer,
            contact.photo.as_ref().map(|photo| photo.0.as_slice()),
        )?;
        self.book.links.insert(
            peer,
            links::Link {
                contact_id: contact.contact_id,
                lookup: contact.lookup.clone(),
                name: name.clone(),
            },
        );
        links::save(&self.home, &self.book)?;
        self.model.names.link(peer, &name, &contact.lookup);
        self.row_cache.get_mut().remove(&peer);
        self.dirty = true;
        Ok(())
    }

    fn unlink(&mut self, peer: [u8; 16]) -> Result<(), String> {
        if self.book.links.remove(&peer).is_none() {
            return Ok(());
        }
        links::save(&self.home, &self.book)?;
        self.model.names.unlink(&peer);
        self.row_cache.get_mut().remove(&peer);
        self.dirty = true;
        self.photos.get_mut().remove(&peer);
        links::keep_photo(&self.home, &peer, None)
    }

    /// The IM rows of protocol LXMF the address book holds: those of an
    /// address loquax knows and has not linked become links. An address
    /// two contacts carry is left for the user to choose. How many were
    /// linked.
    fn relink(&mut self, rows: &[schema::ImRow]) -> Result<usize, String> {
        let mut by_address: std::collections::BTreeMap<[u8; 16], Vec<&schema::ImRow>> =
            std::collections::BTreeMap::new();
        for row in rows {
            if let Ok(peer) = loquax::address::parse(row.address.trim()) {
                by_address.entry(peer).or_default().push(row);
            }
        }
        let mut linked = 0;
        for (peer, rows) in by_address {
            let first = rows[0];
            let one_contact = rows
                .iter()
                .all(|r| r.contact.lookup == first.contact.lookup);
            if !one_contact || self.book.links.contains_key(&peer) || !self.knows(&peer) {
                continue;
            }
            self.link(peer, &first.contact)?;
            linked += 1;
        }
        Ok(linked)
    }

    /// Whether loquax knows `peer`: a conversation, a name heard or given.
    fn knows(&self, peer: &[u8; 16]) -> bool {
        self.model.conversations.iter().any(|c| c.peer == *peer)
            || self.model.names.shown(peer).is_some()
    }

    /// The oldest send, if none runs; the owed pull, if none runs.
    fn start_next(&mut self) {
        if !self.sending {
            if let Some((to, body, fields)) = self.sends.pop_front() {
                self.sending = true;
                let mut tap = self.fanout.tap();
                let client = self.client.clone();
                let done = self.finished_tx.clone();
                let wake = Arc::clone(&self.wake);
                self.runtime.spawn(async move {
                    let event =
                        runner::send_home(&mut tap, &client, to, body, fields, BUDGET, SETTLE)
                            .await;
                    let _ = done.send(Finished::Send(event));
                    wake.notify_one();
                });
            }
        }
        if !self.pulling && std::mem::take(&mut self.pull_owed) {
            self.pulling = true;
            let asked = std::mem::take(&mut self.pull_asked);
            if let Some(say) = self.pull_line.began(self.elapsed_ms(), asked) {
                self.say(say);
            }
            let mut tap = self.fanout.tap();
            let client = self.client.clone();
            let home = self.home.clone();
            let done = self.finished_tx.clone();
            let wake = Arc::clone(&self.wake);
            self.runtime.spawn(async move {
                let end = runner::pull(&mut tap, &client, &home, BUDGET).await;
                // The history it imported, read here, off the session's
                // lock (1464): the store's read was the pull end's turn.
                let stored = match end.imported {
                    0 => None,
                    _ => {
                        let key = client.public_key;
                        let read =
                            tokio::task::spawn_blocking(move || runner::read_imported(&home, &key))
                                .await
                                .map_err(|error| error.to_string());
                        Some(read.and_then(|read| read))
                    }
                };
                let _ = done.send(Finished::Pull(end, stored));
                wake.notify_one();
            });
        }
    }

    /// A pull ended: what it brought is in the store by now (the pump
    /// folded it before the runner saw its fence), so the cursor may pass it.
    /// `result` counts the new messages it brought, never entries. History
    /// it imported (1159) the runner wrote to the store itself; the model
    /// takes it from there, as the session lives on in the service and is
    /// never restarted by closing the app.
    fn pulled(
        &mut self,
        end: runner::PullEnd,
        stored: Option<Result<Vec<loquax::ui::history::Restored>, String>>,
    ) {
        self.pulling = false;
        runner::settle_pull(&self.home, end.cursor);
        // Once a pull settled, what arrives is live mail (1429).
        if end.cursor.is_some() {
            self.model.history = false;
        }
        if let Some(stored) = stored {
            self.persist();
            let shown = stored
                .and_then(|stored| runner::show_restored(&mut self.model, &self.store, stored));
            match shown {
                Ok(()) => tracing::info!("HOME imported rows={} shown", end.imported),
                Err(error) => {
                    tracing::warn!("HOME imported rows={} not shown: {error}", end.imported)
                }
            }
            self.dirty = true;
        }
        if let Some(say) = self.pull_line.ended(&end.result) {
            self.say(say);
        }
        let pulled = end.result.is_ok();
        self.apply(UiEvent::FetchDone(end.result));
        if pulled {
            self.ask_pages();
        }
    }

    /// After a pull (1579): the pages the home heard and the identity's
    /// bookmarks after the stamp the phone holds, the pending bookmark
    /// changes handed first. One ask at a time.
    fn ask_pages(&mut self) {
        if self
            .pages_asking
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let client = self.client.clone();
        let pending = self.model.browse.shared.pending();
        let stamp = self.model.browse.shared.stamp;
        let done = self.finished_tx.clone();
        let wake = Arc::clone(&self.wake);
        tracing::info!("HOME pages asked since={stamp}");
        self.pages_asking = Some(self.runtime.spawn(async move {
            if let Some(event) = runner::home_pages(client, pending, stamp, BUDGET).await {
                let _ = done.send(Finished::Event(event));
                wake.notify_one();
            }
        }));
        loquax::ui::explore::pages_asked(&mut self.model);
    }

    /// The two lists into [`Inner::scene`] (1464), and the digest of the
    /// counts the service notifies by.
    fn show_scene(&mut self) {
        let listed = self.conversations(None);
        self.counts = counts(&listed);
        let conversations = schema::encode(&listed);
        let archived = schema::encode(&self.listed(self.model.archived_order(), None));
        let mut scene = self
            .scene
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        scene.conversations = Some(conversations);
        scene.archived = Some(archived);
        scene.at = Some(self.model.lists_revision());
        scene.builds += 1;
    }

    /// The model's lists revision the lists show.
    fn scene_at(&self) -> Option<u64> {
        self.scene
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .at
    }

    fn conversations(&self, search: Option<&str>) -> schema::Conversations {
        let mut listed = self.listed(self.model.order(), search);
        listed.archived = u32::try_from(self.model.archived_order().len()).unwrap_or(u32::MAX);
        listed
    }

    /// The list's rows for the conversations `order` names, in its order;
    /// with `search` only those it finds ([`chat::row_matches`]), so the
    /// phone searches by the rule every loquax screen does (1217).
    fn listed(&self, order: Vec<usize>, search: Option<&str>) -> schema::Conversations {
        let rows = match search {
            Some(_) => found(&self.model, order, search)
                .into_iter()
                .map(|row| self.schema_row(row))
                .collect(),
            None => self.cached_rows(order),
        };
        let (address, initials) = self.model.address.map_or_else(Default::default, |own| {
            // The own avatar (1452 rule 4): the core's one chain for the
            // own name (1647); never the hash.
            let named = loquax::ui::profile::own_name(&self.model);
            (loquax::address::to_hex(&own), chat::initials(named, &own))
        });
        schema::Conversations {
            v: VERSION,
            conversations: rows,
            address,
            initials,
            archived: 0,
            home_client: self.model.home_client,
            home_link: self.watching,
            offline: self.offline.offline(),
        }
    }

    /// Every blocked peer, with a conversation or without, as list rows
    /// (1146): the settings screen's "Blocked contacts".
    fn blocked(&self) -> schema::Conversations {
        let mut listed = self.listed(Vec::new(), None);
        listed.conversations = self
            .model
            .names
            .contacts
            .blocked
            .iter()
            .map(|peer| self.list_row(peer))
            .collect();
        listed
    }

    /// One row of the list for `peer`, empty when it has no conversation.
    fn list_row(&self, peer: &[u8; 16]) -> schema::ConversationRow {
        self.schema_row(scene::row(&self.model, peer))
    }

    /// A scene's row in the facade's words (1217): what Java saw before
    /// the scene, and the scene's words in the phone's language.
    /// The rows for `order`, each made again only when its stamp moved
    /// ([`Inner::row_stamp`], 1595); the rest are the last list's.
    fn cached_rows(&self, order: Vec<usize>) -> Vec<schema::ConversationRow> {
        let mut cache = self.row_cache.borrow_mut();
        // A peer no longer listed keeps no row.
        if cache.len() > 2 * self.model.conversations.len() + 16 {
            cache.clear();
        }
        order
            .into_iter()
            .map(|index| {
                let peer = self.model.conversations[index].peer;
                let stamp = self.row_stamp(index);
                // The open conversation's row is made every time: what it
                // shows (its draft, 1219) hangs on the chat being open,
                // which no revision says.
                let open = index == self.model.selected;
                match cache.get(&peer) {
                    Some((kept, row)) if *kept == stamp && !open => row.clone(),
                    _ => {
                        let row = self.schema_row(scene::row(&self.model, &peer));
                        cache.insert(peer, (stamp, row.clone()));
                        row
                    }
                }
            })
            .collect()
    }

    /// What a list row of conversation `index` is made from, as the
    /// core's own list window keys its rows (`UiModel::list_stamp`): the
    /// conversation's revision, the model-wide look (names, language, the
    /// minute) and whether it is the open one, with the phone's language
    /// and its address book's link for the peer (link and unlink drop the
    /// row besides).
    fn row_stamp(&self, index: usize) -> u64 {
        let conversation = &self.model.conversations[index];
        let mut stamp = 0xcbf2_9ce4_8422_2325u64;
        for word in [
            conversation.revision(),
            self.model.look_rev(),
            self.lang as u64,
            u64::from(self.book.links.contains_key(&conversation.peer)),
            // Open or not: the row that was open is made again when it
            // closes.
            u64::from(index == self.model.selected),
        ] {
            stamp = (stamp ^ word).wrapping_mul(0x0100_0000_01b3);
        }
        stamp
    }

    fn schema_row(&self, row: scene::Row) -> schema::ConversationRow {
        let live = row
            .live
            .map(|live| live.resolve(self.lang))
            .unwrap_or_default();
        let ring = ring(row.avatar.ring);
        let (pin, pin_label) = pin(row.position.as_ref(), self.lang);
        let draft = row
            .draft
            .map(|draft| draft.resolve(self.lang))
            .unwrap_or_default();
        let sharing = row.sharing;
        let group = row.group.clone();
        let same_name = row
            .same_name
            .as_ref()
            .map(|same| same.resolve(self.lang))
            .unwrap_or_default();
        let row = row.row;
        let time = match (row.at, self.model.now) {
            (Some(at), Some(now)) => scene::list_time(&at, &now).resolve(self.lang),
            _ => String::new(),
        };
        schema::ConversationRow {
            peer: loquax::address::to_hex(&row.peer),
            name: row.name,
            same_name,
            last: row.last,
            ours: row.ours,
            at: row.at.map_or(0, |at| at.unix_secs),
            time,
            unread: u32::try_from(row.unread).unwrap_or(u32::MAX),
            initials: row.initials,
            icon_fg: row.icon.as_ref().map(|(fg, _)| fg.clone()),
            icon_bg: row.icon.map(|(_, bg)| bg),
            kind: row.kind.to_string(),
            reaction: row.reaction,
            state: row.mark.map_or("", chat::Mark::word).to_string(),
            read_at: row.read_at.map_or(0, i64::from),
            photo: self.photo(&row.peer),
            linked: self.book.links.contains_key(&row.peer),
            pinned: row.pinned,
            muted: row.muted,
            updates: u32::try_from(row.updates).unwrap_or(u32::MAX),
            liveness: row.liveness.state.word().to_string(),
            cadence_s: cadence(&row.liveness),
            last_fix_at: row.liveness.last_fix_at.unwrap_or(0),
            live,
            ring,
            pin,
            pin_label,
            draft,
            sharing,
            group_members: group
                .as_ref()
                .map_or(0, |g| u32::try_from(g.members).unwrap_or(u32::MAX)),
            group_words: group
                .as_ref()
                .map(|g| g.words.resolve(self.lang))
                .unwrap_or_default(),
            group_invited: group
                .is_some_and(|g| g.standing == loquax::ui::groups::Standing::Invited),
        }
    }

    /// "New group" or the open member sheet (1307), as the scene has them,
    /// in the phone's language.
    fn group_screen(&self) -> schema::GroupScreen {
        let lang = self.lang;
        let scene = self.model.scene(scene::LayoutClass::Compact);
        if let Some(new) = scene.new_group {
            let naming = new.step == loquax::ui::groups::Step::Name;
            let go = if naming { &new.create } else { &new.next };
            return schema::GroupScreen {
                v: VERSION,
                screen: "new".into(),
                title: new.title.resolve(lang),
                step: if naming { "name" } else { "pick" }.into(),
                line: new.picked.resolve(lang),
                rows: new
                    .rows
                    .iter()
                    .map(|r| schema::GroupRow {
                        peer: loquax::address::to_hex(&r.peer),
                        name: r.name.clone(),
                        picked: r.picked,
                        ..schema::GroupRow::default()
                    })
                    .collect(),
                hint: new.hint.resolve(lang),
                name: new.name.clone(),
                trust: new.trust.resolve(lang),
                go: go.label_key.text(lang).to_string(),
                go_enabled: go.enabled,
                ..schema::GroupScreen::default()
            };
        }
        let Some(sheet) = scene.members else {
            return schema::GroupScreen {
                v: VERSION,
                ..schema::GroupScreen::default()
            };
        };
        schema::GroupScreen {
            v: VERSION,
            screen: "members".into(),
            title: sheet.title.resolve(lang),
            line: sheet.count.resolve(lang),
            rows: sheet
                .rows
                .iter()
                .map(|r| schema::GroupRow {
                    peer: loquax::address::to_hex(&r.address),
                    name: r.name.resolve(lang),
                    state: r.state.resolve(lang),
                    you: r.you,
                    initials: r.avatar.initials.clone(),
                    remove: r
                        .remove
                        .as_ref()
                        .map(|a| a.label_key.text(lang).to_string())
                        .unwrap_or_default(),
                    ..schema::GroupRow::default()
                })
                .collect(),
            leave: sheet
                .leave
                .as_ref()
                .map(|a| a.label_key.text(lang).to_string())
                .unwrap_or_default(),
            add: sheet
                .add
                .as_ref()
                .map(|a| a.label_key.text(lang).to_string())
                .unwrap_or_default(),
            delete: sheet
                .delete
                .as_ref()
                .map(|a| a.label_key.text(lang).to_string())
                .unwrap_or_default(),
            again: sheet
                .again
                .as_ref()
                .map(|a| a.label_key.text(lang).to_string())
                .unwrap_or_default(),
            ask: sheet
                .ask
                .as_ref()
                .map(|a| a.words.resolve(lang))
                .unwrap_or_default(),
            yes: sheet
                .ask
                .as_ref()
                .map(|_| StrId::GroupYes.text(lang).to_string())
                .unwrap_or_default(),
            no: sheet
                .ask
                .as_ref()
                .map(|_| StrId::GroupNo.text(lang).to_string())
                .unwrap_or_default(),
            ..schema::GroupScreen::default()
        }
    }

    /// The running position shares (1273), Settings, Privacy's list.
    fn shares(&self) -> schema::Shares {
        schema::Shares {
            v: VERSION,
            title: Text::key(StrId::PositionSharing).resolve(self.lang),
            empty: Text::key(StrId::NoShares).resolve(self.lang),
            stop: Text::key(StrId::ShareStop).resolve(self.lang),
            shares: scene::share_rows(&self.model)
                .into_iter()
                .map(|row| schema::ShareRow {
                    peer: loquax::address::to_hex(&row.peer),
                    id: row.id.resource(),
                    name: row.name,
                    words: row.words.resolve(self.lang),
                    until: self
                        .model
                        .share(&row.peer)
                        .and_then(|share| share.until)
                        .unwrap_or(0),
                })
                .collect(),
        }
    }

    /// The contact screen of `peer` (1146).
    /// The positions we shared, newest first (1201).
    fn shared(&self) -> Result<schema::Shared, String> {
        shared(&self.model, &self.store, |peer| self.photo(peer))
    }

    fn detail(&self, peer: [u8; 16]) -> Result<schema::Detail, String> {
        let row = self.list_row(&peer);
        let facts = self.model.contact_facts(&peer);
        let (qr_size, modules) = loquax::home::qr::address_modules(&peer)?;
        let link = self.book.links.get(&peer);
        let names = &self.model.names;
        let positions = self
            .store
            .track(peer, POSITIONS_SHOWN)
            .map_err(|error| error.to_string())?
            .iter()
            .map(|fix| {
                let at = WallTime {
                    unix_secs: fix.fix_time(),
                    offset_secs: self.offset,
                };
                schema::PositionRow {
                    at: at.unix_secs,
                    lat_e6: fix.location.latitude_e6,
                    lon_e6: fix.location.longitude_e6,
                    day: scene::day_words(&at, self.model.now.as_ref()).resolve(self.lang),
                }
            })
            .collect();
        Ok(schema::Detail {
            v: VERSION,
            peer: row.peer,
            name: row.name,
            initials: row.initials,
            icon_fg: row.icon_fg,
            icon_bg: row.icon_bg,
            photo: row.photo,
            linked: link.is_some(),
            lookup: link.map(|link| link.lookup.clone()),
            contact_id: link.map(|link| link.contact_id),
            petname: names.petname(&peer).map(str::to_string),
            announced: names.announced(&peer).map(str::to_string),
            fingerprint: fingerprint(&peer),
            qr_size,
            qr: schema::Blob(modules.into_iter().map(u8::from).collect()),
            verified: facts.verified,
            blocked: facts.state.blocked,
            pinned: facts.state.pinned,
            archived: facts.state.archived,
            muted: self.model.muted(&peer),
            muted_until: match facts.state.muted_until {
                None => 0,
                Some(loquax::ui::state::MUTED_ALWAYS) => -1,
                Some(until) => until,
            },
            last_received: facts.last_received.map_or(0, |at| at.unix_secs),
            last_sent: facts
                .last_sent
                .and_then(|(at, _)| at)
                .map_or(0, |at| at.unix_secs),
            last_sent_how: facts
                .last_sent
                .map(|(_, how)| how.to_string())
                .unwrap_or_default(),
            home_client: self.model.home_client,
            positions,
        })
    }

    /// The picture bytes of the message `key` of `peer`'s conversation,
    /// if it carries a photo.
    fn picture(&self, peer: [u8; 16], key: u64) -> Option<Vec<u8>> {
        self.model
            .conversations
            .iter()
            .find(|c| c.peer == peer)?
            .messages
            .iter()
            .find(|m| m.activity == key)
            .and_then(|m| loquax::ui::photos::photo_bytes(&m.fields))
            .map(<[u8]>::to_vec)
    }

    fn messages(&self, peer: [u8; 16]) -> schema::Messages {
        let index = self.model.conversations.iter().position(|c| c.peer == peer);
        let icon =
            index.and_then(|c| chat::latest_appearance(&self.model.conversations[c].messages));
        let rows = index.map_or_else(Vec::new, |c| rows(&self.model, c, self.lang));
        let link = self.book.links.get(&peer);
        let names = &self.model.names;
        let liveness = self.model.liveness(&peer);
        // The field opens with the scene's compose (1219).
        let compose = index.map(|c| scene::compose(&self.model, c));
        let bar = self.bar(peer);
        // A group's chat (1307): the count, and the card while invited.
        let badge = loquax::ui::group_scene::badge(&self.model, &peer);
        let invite = badge
            .as_ref()
            .filter(|b| b.standing == loquax::ui::groups::Standing::Invited)
            .and_then(|_| loquax::ui::group_scene::invite(&self.model, &peer, false));
        schema::Messages {
            v: VERSION,
            peer: loquax::address::to_hex(&peer),
            name: self.model.names.label(&peer, self.lang),
            same_name: self
                .model
                .names
                .same_name(&peer)
                .map(|same| same.resolve(self.lang))
                .unwrap_or_default(),
            relayed: relayed_title(&self.model, &peer, self.lang),
            messages: rows,
            initials: self.initials(&peer),
            icon_fg: icon.as_ref().map(|(fg, _)| fg.clone()),
            icon_bg: icon.map(|(_, bg)| bg),
            home_client: self.model.home_client,
            photo: self.photo(&peer),
            linked: link.is_some(),
            lookup: link.map(|link| link.lookup.clone()),
            contact_id: link.map(|link| link.contact_id),
            // A group is not a person (1307 pass six): only a person who
            // wrote is offered for the phone's contacts.
            ask_link: link.is_none()
                && self.book.met_here(
                    self.model
                        .first_word_from_a_person(&peer)
                        .map(|at| at.unix_secs),
                )
                && !self.book.not_now.contains(&peer)
                && !self.just_added.contains(&peer),
            announced: names.announced(&peer).map(str::to_string),
            petname: names.petname(&peer).map(str::to_string),
            // The contact's other addresses, in the contact info where an
            // address is looked up: each with its short hash, which tells
            // them apart while their names are one (1452).
            others: names
                .same_contact(&peer)
                .iter()
                .map(|other| {
                    format!(
                        "{} {}",
                        names.label(other, self.lang),
                        loquax::names::hash_suffix(other)
                    )
                })
                .collect(),
            verified: self.model.verified(&peer),
            receipts: index.is_some_and(|c| self.model.conversations[c].flags.receipts),
            liveness: liveness.state.word().to_string(),
            cadence_s: cadence(&liveness),
            last_fix_at: liveness.last_fix_at.unwrap_or(0),
            ring: ring(scene::Ring::of(liveness.state)),
            draft: compose.as_ref().map(|c| c.text.clone()).unwrap_or_default(),
            draft_reply: compose
                .as_ref()
                .and_then(|c| c.reply)
                .map(|id| loquax::address::to_hex(&id))
                .unwrap_or_default(),
            round: bar.round,
            buttons: bar.buttons,
            hint: bar.hint,
            staged: bar.staged,
            sheet: bar.sheet,
            sheet_title: bar.sheet_title,
            sheet_rows: bar.sheet_rows,
            has_fix: bar.has_fix,
            fix_lat_e6: bar.fix_lat_e6,
            fix_lon_e6: bar.fix_lon_e6,
            fix_words: bar.fix_words,
            share: scene::share_chip(&self.model, &peer)
                .map(|chip| chip.words.resolve(self.lang))
                .unwrap_or_default(),
            share_stop: Text::key(StrId::ShareStop).resolve(self.lang),
            recording: bar.recording,
            group_members: badge.map_or(0, |b| u32::try_from(b.members).unwrap_or(u32::MAX)),
            invite_title: invite
                .as_ref()
                .map(|i| i.title.resolve(self.lang))
                .unwrap_or_default(),
            invite_trust: invite
                .map(|i| i.trust.resolve(self.lang))
                .unwrap_or_default(),
            ended: loquax::ui::group_scene::ended(&self.model, &peer)
                .map(|t| t.resolve(self.lang))
                .unwrap_or_default(),
        }
    }

    /// `peer`'s compose bar (1270, the scene's compose node and the sheet
    /// it opened), for [`schema::Messages`] and [`schema::Query::Bar`].
    fn bar(&self, peer: [u8; 16]) -> schema::Bar {
        let compose = self
            .model
            .conversations
            .iter()
            .position(|c| c.peer == peer)
            .map(|c| scene::compose(&self.model, c));
        let mut bar = schema::Bar {
            v: VERSION,
            ..schema::Bar::default()
        };
        if let Some(compose) = compose {
            bar.recording = compose
                .recording
                .as_ref()
                .map(|r| r.phase.word().to_string())
                .unwrap_or_default();
            bar.round = compose.round.word().to_string();
            bar.buttons = compose
                .buttons
                .iter()
                .map(|b| schema::BarButton {
                    id: b.id.resource(),
                    label: b.a11y.label.resolve(self.lang),
                })
                .collect();
            bar.hint = compose.a11y.label.resolve(self.lang);
            bar.staged = compose
                .staged
                .as_ref()
                .map(|st| st.words.resolve(self.lang))
                .unwrap_or_default();
        }
        let (sheet, whereabouts) = scene::bar_sheet(&self.model, peer);
        let Some(sheet) = sheet else {
            return bar;
        };
        let fix = whereabouts.as_ref().and_then(|w| w.fix);
        bar.sheet = self
            .model
            .bar
            .sheet
            .map(|s| s.word().to_string())
            .unwrap_or_default();
        bar.sheet_title = sheet.title.resolve(self.lang);
        bar.sheet_rows = sheet
            .actions
            .iter()
            .filter_map(|a| {
                Some(schema::BarButton {
                    id: scene::NodeId::SheetRow(scene::sheet_row(&a.intent)?).resource(),
                    label: Text::key(a.label_key).resolve(self.lang),
                })
            })
            .collect();
        bar.has_fix = fix.is_some();
        bar.fix_lat_e6 = fix.map_or(0, |f| f.0);
        bar.fix_lon_e6 = fix.map_or(0, |f| f.1);
        bar.fix_words = whereabouts
            .map(|w| w.words.resolve(self.lang))
            .unwrap_or_default();
        bar
    }

    /// A linked peer's copied thumbnail, asked of the file system once
    /// until the link changes (1496).
    fn photo(&self, peer: &[u8; 16]) -> Option<String> {
        if !self.book.links.contains_key(peer) {
            return None;
        }
        if let Some(known) = self.photos.borrow().get(peer) {
            return known.clone();
        }
        self.photo_stats.set(self.photo_stats.get() + 1);
        let found = links::photo(&self.home, peer);
        self.photos.borrow_mut().insert(*peer, found.clone());
        found
    }

    /// `emoji` was used: first in the recents kept in the home. A file that
    /// cannot be written costs the order, not the reaction.
    fn used(&self, emoji: &str) {
        let mut recents = loquax::emoji::load_recents(&self.home);
        recents.used(emoji);
        if let Err(error) = loquax::emoji::save_recents(&self.home, &recents) {
            tracing::warn!("{error}");
        }
    }

    fn initials(&self, peer: &[u8; 16]) -> String {
        chat::initials(self.model.names.shown(peer), peer)
    }
}

/// A model that knows what the store and the contacts file know of the
/// peers: their names and their latest positions. The session's starts so,
/// and the map draws its pins from one when no session runs.
pub fn known(home: &Path, store: &Store) -> Result<UiModel<Line>, String> {
    let contacts = loquax::names::load_contacts(home).map_err(|error| error.to_string())?;
    let mut model = UiModel::<Line>::new(&[]);
    // The phone scrolls the browser's page natively (1650): the scene
    // carries it whole, up to the cap.
    model.browse.model.window = Some(crate::browser::WHOLE_PAGE);
    model.names.contacts = contacts;
    for (peer, name) in store.names().map_err(|error| error.to_string())? {
        model.names.remember(peer, &name);
    }
    for (peer, link) in links::load(home).links {
        model.names.link(peer, &link.name, &link.lookup);
    }
    model.restore_positions(store.positions().map_err(|error| error.to_string())?);
    model.restore_tracks(
        store
            .fixes_between(None, i64::MIN, i64::MAX)
            .map_err(|error| error.to_string())?,
    );
    Ok(model)
}

/// The candidates `peer`'s announced name may be, best first; none for a
/// peer that announced none.
pub fn suggest(
    names: &loquax::names::Names,
    peer: &[u8; 16],
    rows: &[schema::CandidateRow],
) -> schema::Suggestions {
    use loquax::name_match::{rank, Candidate, Field};
    let candidates: Vec<Candidate> = rows
        .iter()
        .map(|row| Candidate {
            key: row.key.clone(),
            display: row.display.clone(),
            given: row.given.clone(),
            middle: row.middle.clone(),
            family: row.family.clone(),
            nicknames: row.nicknames.clone(),
            last_contacted: row.last_contacted,
        })
        .collect();
    let ranked = names
        .announced(peer)
        .map(|announced| rank(announced, &candidates))
        .unwrap_or_default();
    schema::Suggestions {
        v: VERSION,
        suggestions: ranked
            .into_iter()
            .map(|s| {
                let (field, nickname) = match s.field {
                    Field::Display => ("display", None),
                    Field::Nickname(nick) => ("nickname", Some(nick)),
                    Field::Name => ("name", None),
                    Field::Given => ("given", None),
                    Field::Words => ("words", None),
                    Field::Prefix => ("prefix", None),
                };
                schema::SuggestionRow {
                    key: s.key,
                    score: u32::from(s.score),
                    field: field.to_string(),
                    nickname,
                }
            })
            .collect(),
    }
}

/// The map's pins as a model `known` from `home`'s store names them, at
/// the wall clock `now`; none when the phone is no client yet.
pub fn pins_in(home: &Path, now: WallTime) -> Result<Vec<MapPin>, String> {
    let Some(key) = loquax::home::client::load_client(home)? else {
        return Ok(Vec::new());
    };
    let address = loquax::home::lxmf_address(&key)?;
    let path = home.join(loquax::store::STORE_FILE);
    let Some(store) = Store::open_read_only(&path, address).map_err(|e| e.to_string())? else {
        return Ok(Vec::new());
    };
    let mut model = known(home, &store)?;
    update(&mut model, UiEvent::Clock(now));
    Ok(model.map_pins())
}

/// The scene's rows for the conversations `order` names, in its order,
/// those `search` finds when given: every word of it in the name or the
/// last line ([`chat::row_matches`], the desktop's rule too).
pub fn found<B>(model: &UiModel<B>, order: Vec<usize>, search: Option<&str>) -> Vec<scene::Row> {
    let mut rows = scene::rows(model, order);
    if let Some(search) = search {
        rows.retain(|row| chat::row_matches(&row.row, search));
    }
    rows
}

/// The bubbles of conversation `c`, as the app draws them: the scene's
/// chat ([`scene::chat`], from [`chat::bubbles`]) with its groups laid
/// flat, each bubble with where it sits and its words in `lang`. A
/// reaction is a chip on the bubble it names, not a bubble, and a report
/// with a position is the position line alone.
pub fn rows<B>(model: &UiModel<B>, c: usize, lang: Lang) -> Vec<schema::MessageRow> {
    let groups = scene::chat(model, c).groups;
    let last = groups.len().saturating_sub(1);
    let mut rows = Vec::new();
    for (g, group) in groups.into_iter().enumerate() {
        let mut day = group.day.map(|day| day.words.resolve(lang));
        for node in group.bubbles {
            let photo = node.photo.as_ref().map(|photo| {
                let shown = model.conversations[c]
                    .messages
                    .iter()
                    .find(|m| m.activity == node.bubble.key)
                    .and_then(|m| loquax::ui::photos::photo_bytes(&m.fields))
                    .and_then(loquax::photo::dimensions)
                    .unwrap_or((0, 0));
                (
                    photo.format.clone(),
                    photo.bytes,
                    shown,
                    photo.words.resolve(lang),
                )
            });
            let files = node
                .files
                .iter()
                .map(|file| schema::FileRow {
                    name: file.name.clone(),
                    bytes: u32::try_from(file.bytes).unwrap_or(u32::MAX),
                    size: file.size.clone(),
                    words: file.words.resolve(lang),
                })
                .collect();
            // A group's writer above the first bubble of its group, and an
            // invitation's card (1307).
            let writer = match (group.named, node.place.joins_before) {
                (true, false) => group.sender.resolve(lang),
                _ => String::new(),
            };
            let invite = node.invite.clone();
            let late = node.bubble.late;
            let state_words = node.mark.as_ref().map(|m| m.words.resolve(lang));
            let position_words = node.position.as_ref().map(|p| p.resolve(lang));
            let mut row = row(node.bubble);
            row.state_words = state_words.unwrap_or_default();
            row.position_words = position_words.unwrap_or_default();
            if late {
                row.late = StrId::GroupArrivedLate.text(lang).to_string();
            }
            row.writer = writer;
            if let Some(invite) = invite {
                row.invite_group = loquax::address::to_hex(&invite.group);
                row.invite_title = invite.title.resolve(lang);
                row.invite_trust = invite.trust.resolve(lang);
                row.invite_state = invite.state.map(|s| s.resolve(lang)).unwrap_or_default();
                row.invite_join = invite.join.is_some();
            }
            // The scene's words (1268): a body of blanks is none, and the
            // app draws no text row under a photo without words.
            row.body = node.text.unwrap_or_default();
            row.files = files;
            if let Some(voice) = &node.voice {
                row.voice_mode = u32::from(voice.mode);
                row.voice_ms = voice.duration_ms;
                row.voice_duration = voice.duration.clone();
                row.voice_levels = schema::Blob(voice.levels.clone());
                row.voice_playable = voice.toggle.is_some();
                row.voice_playing = voice.playing;
                row.voice_words = voice.words.resolve(lang);
            }
            if let Some((format, bytes, (w, h), words)) = photo {
                row.photo = format;
                row.photo_bytes = u32::try_from(bytes).unwrap_or(u32::MAX);
                row.photo_w = w;
                row.photo_h = h;
                row.photo_words = words;
            }
            row.joins_before = node.place.joins_before;
            row.joins_after = node.place.joins_after;
            row.meta = node.place.meta;
            row.newest = g == last;
            row.day = day.take().unwrap_or_default();
            row.live = node.live.map(|live| live.resolve(lang)).unwrap_or_default();
            rows.push(row);
        }
    }
    rows
}

/// A ring as the facade carries it: its colour's name, empty for none.
/// The bar's title of a relay's chat (1482), "ReticulumNet NL · Gruppe
/// über Relais", as [`schema::Messages::relayed`] carries it; empty for
/// every other chat.
pub fn relayed_title<B>(model: &UiModel<B>, peer: &[u8; 16], lang: Lang) -> String {
    let relayed = model.group(peer).is_none()
        && model
            .conversations
            .iter()
            .find(|c| c.peer == *peer)
            .is_some_and(loquax::ui::relayed::is_relayed);
    match relayed {
        true => Text::Key(
            StrId::RelayedGroup,
            Vec::from([loquax::ui::strings::Arg::Str(model.names.label(peer, lang))]),
        )
        .resolve(lang),
        false => String::new(),
    }
}

pub fn ring(ring: Option<scene::Ring>) -> String {
    ring.map_or("", scene::Ring::word).to_string()
}

/// A row's map pin as [`schema::ConversationRow::pin`] and `pin_label`
/// carry it (1224): its tint's word and its content description in
/// `lang`, both empty for none.
pub fn pin(position: Option<&scene::PositionBadge>, lang: Lang) -> (String, String) {
    position.map_or_else(Default::default, |pin| {
        (pin.tint.word().to_string(), pin.a11y.label.resolve(lang))
    })
}

/// One bubble in the facade's words, placed by [`rows`].
fn row(bubble: chat::Bubble) -> schema::MessageRow {
    let fix = bubble.fix.as_ref();
    schema::MessageRow {
        id: bubble
            .id
            .map(|id| loquax::address::to_hex(&id))
            .unwrap_or_default(),
        ours: bubble.ours,
        body: bubble.body,
        state: bubble.mark.map_or("", chat::Mark::word).to_string(),
        read_at: bubble.read_at.map_or(0, i64::from),
        at: bubble.at.map_or(0, |at| at.unix_secs),
        chips: bubble
            .chips
            .into_iter()
            .map(|chip| schema::Chip {
                content: chip.content,
                count: u32::try_from(chip.count).unwrap_or(u32::MAX),
                ours: chip.ours,
            })
            .collect(),
        lat_e6: fix.map(|f| f.location.latitude_e6),
        lon_e6: fix.map(|f| f.location.longitude_e6),
        place: fix.map(|f| loquax::telemetry::coordinates(&f.location)),
        accuracy: fix.and_then(|f| loquax::telemetry::accuracy(&f.location)),
        fix_at: fix.map(|f| f.order_time()),
        stale_at: fix.map(|f| {
            f.fix_time()
                .saturating_add(loquax::telemetry::STALE_AFTER_SECS)
        }),
        key: schema::wire_key(bubble.key),
        quote_ours: bubble.quote.as_ref().and_then(|q| q.ours),
        quote_to: bubble
            .quote
            .as_ref()
            .map(|q| loquax::address::to_hex(&q.to)),
        quote: bubble.quote.map(|q| q.text),
        updates: u32::try_from(bubble.updates).unwrap_or(u32::MAX),
        since: bubble.since.map_or(0, |at| at.unix_secs),
        liveness: bubble.liveness.map_or("", |l| l.state.word()).to_string(),
        cadence_s: bubble.liveness.map_or(0, |l| cadence(&l)),
        joins_before: false,
        joins_after: false,
        meta: false,
        newest: false,
        day: String::new(),
        live: String::new(),
        photo: String::new(),
        photo_bytes: 0,
        photo_w: 0,
        photo_h: 0,
        photo_words: String::new(),
        files: Vec::new(),
        voice_mode: 0,
        voice_ms: 0,
        voice_duration: String::new(),
        voice_levels: schema::Blob::default(),
        voice_playable: false,
        voice_playing: false,
        voice_words: String::new(),
        writer: String::new(),
        invite_group: String::new(),
        invite_title: String::new(),
        invite_trust: String::new(),
        invite_state: String::new(),
        invite_join: false,
        note: bubble.note,
        late: String::new(),
        state_words: String::new(),
        position_words: String::new(),
    }
}

/// A liveness's cadence as the facade carries it: seconds, 0 before one
/// was learned.
pub fn cadence(liveness: &Liveness) -> u32 {
    liveness
        .cadence_s
        .map_or(0, |s| u32::try_from(s).unwrap_or(u32::MAX))
}

/// The reports the row `key` of `peer`'s conversation stands for, newest
/// first ([`schema::Query::Run`]).
pub fn run<B>(model: &UiModel<B>, peer: [u8; 16], key: u64) -> schema::Run {
    let found = model
        .conversations
        .iter()
        .enumerate()
        .find_map(|(c, conv)| {
            (conv.peer == peer)
                .then(|| conv.messages.iter().position(|m| m.activity == key))
                .flatten()
                .map(|m| (c, conv.run(m)))
        });
    let Some((c, run)) = found else {
        return schema::Run {
            v: VERSION,
            positions: Vec::new(),
        };
    };
    let messages = &model.conversations[c].messages;
    schema::Run {
        v: VERSION,
        positions: run
            .into_iter()
            .rev()
            .map(|m| {
                let fix = model.message_fix(c, m);
                let fix = fix.as_ref();
                schema::RunRow {
                    key: schema::wire_key(messages[m].activity),
                    at: messages[m].at.map_or(0, |at| at.unix_secs),
                    lat_e6: fix.map(|f| f.location.latitude_e6),
                    lon_e6: fix.map(|f| f.location.longitude_e6),
                    place: fix.map(|f| loquax::telemetry::coordinates(&f.location)),
                    accuracy: fix.and_then(|f| loquax::telemetry::accuracy(&f.location)),
                    fix_at: fix.map(|f| f.order_time()),
                }
            })
            .collect(),
    }
}

/// The privacy page's list (1201): the positions `store` says we sent,
/// newest first, each with its recipient as `model`'s list draws them.
pub fn shared<B>(
    model: &UiModel<B>,
    store: &Store,
    photo: impl Fn(&[u8; 16]) -> Option<String>,
) -> Result<schema::Shared, String> {
    let shared = store
        .shared(SHARED_SHOWN)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(fix, sent)| {
            let row = chat::list_row(model, &fix.via);
            schema::SharedRow {
                peer: loquax::address::to_hex(&fix.via),
                name: row.name,
                initials: row.initials,
                icon_fg: row.icon.as_ref().map(|(fg, _)| fg.clone()),
                icon_bg: row.icon.map(|(_, bg)| bg),
                photo: photo(&fix.via),
                at: sent.unwrap_or_else(|| fix.fix_time()),
                lat_e6: fix.location.latitude_e6,
                lon_e6: fix.location.longitude_e6,
            }
        })
        .collect();
    Ok(schema::Shared { v: VERSION, shared })
}

/// The fix the app hands in for a share (1201), as the wire carries it;
/// a place off the globe is refused, so a broken reading never leaves the
/// phone.
/// A facade request decoded at the boundary (1217): the user's intent, or
/// the phone's address book glue, which is no intent.
pub enum Decoded {
    Intent(Intent),
    Platform(Platform),
}

/// The requests that are the phone's own glue.
pub enum Platform {
    Link {
        peer: [u8; 16],
        contact: schema::ContactCopy,
    },
    Unlink {
        peer: [u8; 16],
    },
    NotNow {
        peer: [u8; 16],
    },
    Relink {
        rows: Vec<schema::ImRow>,
    },
    /// The microphone's gesture or the recorder's level (1276): the
    /// platform's input, not an intent (`loquax::ui::recording::Input`).
    Record {
        peer: [u8; 16],
        input: Input,
    },
    /// A verb of the Browse tab (1337), as the model's event: lnomad's
    /// intent carried, the tab switch, or the page view's size.
    Browse(UiEvent),
    /// A verb of the Rooms tab (1384), as the model's events: the tab
    /// switch, a room opened, or the input line typed and sent; and one of
    /// the Explore tab (1578), which are model events the same way.
    Rooms(Vec<UiEvent>),
}

/// `request` as an intent, with its addresses and ids parsed: every
/// facade verb is one ([`loquax::ui::intent`]'s table names each).
pub fn decode(request: schema::Request) -> Result<Decoded, String> {
    use schema::Request as R;
    let intent = match request {
        R::Open { peer } => Intent::OpenChat {
            peer: parse_peer(&peer)?,
        },
        R::Close => Intent::CloseChat,
        R::Send { peer, body } => Intent::Send {
            to: Target::Peer(parse_peer(&peer)?),
            body: Some(body),
        },
        R::React {
            peer,
            target,
            content,
        } => Intent::React {
            to: Target::Peer(parse_peer(&peer)?),
            target: Some(parse_id(&target)?),
            content: Some(content),
        },
        R::Reply { peer, target, body } => Intent::Reply {
            to: Target::Peer(parse_peer(&peer)?),
            target: parse_id(&target)?,
            body: Some(body),
        },
        R::Forward { peer, key, to } => Intent::Forward {
            from: parse_peer(&peer)?,
            key: schema::model_key(key),
            to: parse_peer(&to)?,
        },
        R::SendAgain { peer, key } => Intent::SendAgain {
            peer: parse_peer(&peer)?,
            key: schema::model_key(key),
        },
        R::Photo { peer, data } => Intent::SendPhoto {
            peer: parse_peer(&peer)?,
            data: data.0,
        },
        R::File {
            peer,
            name,
            data,
            size,
        } => Intent::SendFile {
            peer: parse_peer(&peer)?,
            name,
            data: data.0,
            size,
        },
        R::Voice { peer, pcm, rate } => Intent::SendVoice {
            peer: parse_peer(&peer)?,
            pcm: pcm
                .0
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]))
                .collect(),
            rate,
        },
        R::PlayVoice { peer, key, on } => {
            let (peer, key) = (parse_peer(&peer)?, schema::model_key(key));
            match on {
                true => Intent::PlayVoice { peer, key },
                false => Intent::PauseVoice { peer, key },
            }
        }
        R::Delete { peer, keys } => Intent::Delete {
            messages: Messages::Keys {
                peer: parse_peer(&peer)?,
                keys: keys.into_iter().map(schema::model_key).collect(),
            },
            confirmed: true,
        },
        R::Used { emoji } => Intent::Insert(emoji),
        R::Pull => Intent::Pull,
        R::Front { front } => Intent::Front(front),
        R::Network { kind, changed } => Intent::Network {
            kind: loquax::home::NetworkKind::from_word(&kind),
            changed,
        },
        R::Viewport { peer, keys } => Intent::Viewport {
            peer: parse_peer(&peer)?,
            keys: keys.into_iter().map(schema::model_key).collect(),
        },
        R::Name { name } => Intent::Name(name),
        // The profile's activity is the scene's renderer (1443): it comes
        // with the avatar's intent and leaves with Back.
        R::Profile { open: true } => Intent::Profile,
        R::Profile { open: false } => Intent::Back,
        R::Link { peer, contact } => {
            return Ok(Decoded::Platform(Platform::Link {
                peer: parse_peer(&peer)?,
                contact,
            }))
        }
        R::Unlink { peer } => {
            return Ok(Decoded::Platform(Platform::Unlink {
                peer: parse_peer(&peer)?,
            }))
        }
        R::NotNow { peer } => {
            return Ok(Decoded::Platform(Platform::NotNow {
                peer: parse_peer(&peer)?,
            }))
        }
        R::Relink { rows } => return Ok(Decoded::Platform(Platform::Relink { rows })),
        R::Browse {
            verb,
            node,
            text,
            cols,
            rows,
        } => {
            return crate::browser::event(&verb, &node, &text, cols, rows)
                .map(|event| Decoded::Platform(Platform::Browse(event)))
        }
        R::Rooms { verb, text, index } => {
            return crate::rooms::events(&verb, &text, index)
                .map(|events| Decoded::Platform(Platform::Rooms(events)))
        }
        // Explore (1578): its verbs are the model's events, as the Rooms'.
        R::Explore {
            verb,
            text,
            dest,
            index,
        } => {
            return crate::explore::events(&verb, &text, &dest, index)
                .map(|events| Decoded::Platform(Platform::Rooms(events)))
        }
        R::Rename { peer, name } => Intent::Rename {
            peer: Target::Peer(parse_peer(&peer)?),
            name: Some(name),
        },
        R::Pin { peer, on } => Intent::Pin {
            peer: Target::Peer(parse_peer(&peer)?),
            on: Switch::of(on),
        },
        R::Mute { peer, secs } => Intent::Mute {
            peer: Target::Peer(parse_peer(&peer)?),
            secs,
        },
        R::Archive { peer, on } => Intent::Archive {
            peer: Target::Peer(parse_peer(&peer)?),
            on: Switch::of(on),
        },
        R::Block { peer, on } => Intent::Block {
            peer: Target::Peer(parse_peer(&peer)?),
            on: Switch::of(on),
        },
        R::Verify { peer, on } => Intent::Verify {
            peer: Target::Peer(parse_peer(&peer)?),
            on: Switch::of(on),
        },
        R::Unread { peer, on } => Intent::MarkUnread {
            peer: Target::Peer(parse_peer(&peer)?),
            on: Switch::of(on),
        },
        R::Forget { peer, everywhere } => Intent::Forget {
            peer: Target::Peer(parse_peer(&peer)?),
            everywhere,
            confirmed: true,
        },
        R::Share {
            peer,
            lat_e6,
            lon_e6,
            alt_e2,
            accuracy_e2,
            at,
        } => Intent::Share {
            peer: parse_peer(&peer)?,
            location: share_location(lat_e6, lon_e6, alt_e2, accuracy_e2, at)?,
        },
        R::AddContact { lxma, name } => Intent::AddContact { lxma, name },
        R::Group {
            verb,
            peer,
            member,
            text,
            confirmed,
        } => {
            use loquax::ui::groups::Verb;
            let group = || parse_peer(&peer).map(Target::Peer);
            Intent::Group(match verb.as_str() {
                "new" => Verb::New,
                "pick" => Verb::Pick(parse_peer(&peer)?),
                "next" => Verb::Next,
                "name" => Verb::Name(text),
                "create" => Verb::Create,
                "join" => Verb::Join { group: group()? },
                "members" => Verb::Members { group: group()? },
                "leave" => Verb::Leave {
                    group: group()?,
                    confirmed,
                },
                "remove" => Verb::Remove {
                    group: group()?,
                    member: Some(parse_peer(&member)?),
                    confirmed,
                },
                "add" => Verb::Add { group: group()? },
                "delete" => Verb::Delete {
                    group: group()?,
                    confirmed,
                },
                "again" => Verb::Again { group: group()? },
                "close" => Verb::Close,
                other => return Err(format!("no group verb {other:?}")),
            })
        }
        R::Draft { peer, body, reply } => Intent::Draft {
            to: Target::Peer(parse_peer(&peer)?),
            text: body,
            reply: match reply.is_empty() {
                true => None,
                false => Some(parse_id(&reply)?),
            },
        },
        R::Stage { peer, data } => Intent::StagePhoto {
            peer: parse_peer(&peer)?,
            data: data.0,
        },
        R::Unstage { peer } => Intent::Unstage {
            peer: parse_peer(&peer)?,
        },
        R::Sheet { peer, kind } => match kind.as_str() {
            "" => Intent::CloseSheet,
            "attach" => Intent::Attach {
                peer: parse_peer(&peer)?,
            },
            "position" => Intent::Locate {
                peer: parse_peer(&peer)?,
            },
            other => return Err(format!("{other:?} is not attach, position or empty")),
        },
        R::Fix {
            lat_e6,
            lon_e6,
            alt_e2,
            accuracy_e2,
            at,
        } => Intent::OwnFix {
            location: Some(share_location(lat_e6, lon_e6, alt_e2, accuracy_e2, at)?),
        },
        R::NoFix => Intent::OwnFix { location: None },
        R::ShareFix { peer } => Intent::ShareFix {
            peer: parse_peer(&peer)?,
        },
        R::StartShare { peer, span } => Intent::StartShare {
            peer: parse_peer(&peer)?,
            span: loquax::ui::share::Span::of_word(&span)
                .ok_or_else(|| format!("{span:?} is not 15_min, 1_h, 8_h or until_stopped"))?,
        },
        R::StopShare { peer } => Intent::StopShare {
            peer: parse_peer(&peer)?,
        },
        R::Record {
            peer,
            input,
            now,
            dx,
            dy,
            rms,
        } => {
            return Ok(Decoded::Platform(Platform::Record {
                peer: parse_peer(&peer)?,
                input: record_input(&input, now, dx, dy, rms)?,
            }))
        }
        R::Recording { peer, verb } => Intent::Recording {
            peer: parse_peer(&peer)?,
            verb: verb_of(&verb)
                .ok_or_else(|| format!("{verb:?} is no button of the recording bar"))?,
        },
        // Java asked its question before it sends this (1290).
        R::DeleteRegions => Intent::DeleteAllRegions { confirmed: true },
    };
    Ok(Decoded::Intent(intent))
}

/// The recording bar's verbs (1276), each by its resource id.
const VERBS: [Verb; 8] = [
    Verb::Lock,
    Verb::Pause,
    Verb::Resume,
    Verb::Play,
    Verb::StopPlay,
    Verb::Send,
    Verb::Discard,
    Verb::AskMicrophone,
];

/// The verb whose resource id is `word` (`rec_lock` …, `rec_allow`).
pub fn verb_of(word: &str) -> Option<Verb> {
    VERBS.into_iter().find(|verb| verb.word() == word)
}

/// [`schema::Request::Record`]'s input: its word and the numbers that
/// word takes. A time before the phone's clock began is 0, an RMS past
/// 16 bits full scale.
pub fn record_input(input: &str, now: i64, dx: i32, dy: i32, rms: u32) -> Result<Input, String> {
    let now = u64::try_from(now).unwrap_or(0);
    Ok(match input {
        "press" => Input::Press { now },
        "slide" => Input::Slide { dx, dy },
        "release" => Input::Release { now },
        "cancel" => Input::Cancel,
        "level" => Input::Level {
            now,
            rms: u16::try_from(rms).unwrap_or(u16::MAX),
        },
        "tick" => Input::Tick { now },
        "played" => Input::Played,
        other => {
            return Err(format!(
                "{other:?} is not press, slide, release, cancel, level, tick or played"
            ))
        }
    })
}

/// What the recorder is to do as the facade says it ([`schema::Recording::effects`]).
pub fn effect_word(effect: Effect) -> &'static str {
    match effect {
        Effect::Haptic(Haptic::Start) => "haptic_start",
        Effect::Haptic(Haptic::Lock) => "haptic_lock",
        Effect::Haptic(Haptic::Cap) => "haptic_cap",
        Effect::Start => "start",
        Effect::Pause => "pause",
        Effect::Resume => "resume",
        Effect::Stop => "stop",
        Effect::Finish => "finish",
        Effect::Discard => "discard",
        Effect::Play => "play",
        Effect::StopPlay => "stop_play",
        Effect::AskMicrophone => "ask_microphone",
    }
}

/// `peer`'s recording bar in `lang` (1276, the scene's
/// [`scene::recording_bar`]), with `effects` for the recorder.
pub fn recording<B>(
    model: &UiModel<B>,
    peer: [u8; 16],
    lang: Lang,
    effects: Vec<String>,
) -> schema::Recording {
    use loquax::ui::recording as rec;
    let phase = match model.recording.peer == peer {
        true => model.recording.phase.word(),
        false => rec::Phase::Idle.word(),
    };
    let mut answer = schema::Recording {
        v: VERSION,
        ok: true,
        phase: phase.to_string(),
        effects,
        cancel_dp: rec::CANCEL_DP.unsigned_abs(),
        lock_dp: rec::LOCK_DP.unsigned_abs(),
        window_ms: rec::WINDOW_MS,
        wave_slots: u32::try_from(rec::LIVE_WINDOWS).unwrap_or(u32::MAX),
        longest_ms: loquax::ui::voice::LONGEST_MS,
        ..schema::Recording::default()
    };
    let Some(bar) = scene::recording_bar(model, peer) else {
        return answer;
    };
    answer.shown = true;
    answer.pulsing = bar.pulsing;
    answer.timer = bar.timer.words.resolve(lang);
    answer.timer_ms = bar.timer.ms;
    answer.amber = bar.timer.amber;
    answer.wave = schema::Blob(bar.wave.levels);
    answer.moving = bar.wave.moving;
    answer.wave_label = bar.wave.a11y.label.resolve(lang);
    if let Some(cancel) = &bar.cancel {
        answer.cancel = cancel.words.resolve(lang);
        answer.cancel_progress = u32::from(cancel.progress);
        answer.armed = cancel.armed;
    }
    if let Some(held) = &bar.held {
        answer.held = held.words.resolve(lang);
    }
    if let Some(lock) = &bar.lock {
        answer.lock = lock.a11y.label.resolve(lang);
        answer.lock_closed = lock.closed;
        answer.lock_progress = u32::from(lock.progress);
    }
    answer.buttons = bar
        .buttons
        .iter()
        .map(|b| schema::BarButton {
            id: b.id.resource(),
            label: b.a11y.label.resolve(lang),
        })
        .collect();
    if let Some(notice) = &bar.notice {
        answer.notice = notice.words.resolve(lang);
        if notice.action.is_some() {
            answer.notice_allow = Text::key(Verb::AskMicrophone.label()).resolve(lang);
        }
    }
    answer
}

pub fn share_location(
    lat_e6: i32,
    lon_e6: i32,
    alt_e2: i32,
    accuracy_e2: u32,
    at: i64,
) -> Result<leviculum_lxmf::telemetry::Location, String> {
    if !(-90_000_000..=90_000_000).contains(&lat_e6)
        || !(-180_000_000..=180_000_000).contains(&lon_e6)
    {
        return Err(format!("no place on Earth: {lat_e6}, {lon_e6}"));
    }
    Ok(leviculum_lxmf::telemetry::Location::saturating(
        i64::from(lat_e6),
        i64::from(lon_e6),
        i64::from(alt_e2),
        0,
        0,
        i64::from(accuracy_e2),
        at,
    ))
}

/// What [`schema::Query::Lxma`] answers for `text` ([`loquax::code::read`],
/// 1263): its kind, whose, and the code to hand on; a text that is none
/// refused in one sentence of `lang`. `known` names an address.
pub fn read_code(
    text: &str,
    lang: Lang,
    known: impl Fn(&[u8; 16]) -> String,
) -> Result<schema::Lxma, String> {
    use loquax::code::Code;
    let code = loquax::code::read(text).map_err(|why| why.say(lang).to_string())?;
    let address = code.address();
    Ok(schema::Lxma {
        v: VERSION,
        kind: code.kind().to_string(),
        address: address
            .as_ref()
            .map(|address| loquax::address::to_hex(address))
            .unwrap_or_default(),
        lxma: match &code {
            Code::Contact(contact) => {
                loquax::lxma::encode_parts(&contact.address, &contact.public_key)
            }
            Code::Address(address) => loquax::address::to_hex(address),
            Code::Mailbox(text) => text.clone(),
        },
        known: address.as_ref().map(known).unwrap_or_default(),
    })
}

/// Whether folding `event` changes what the screens show though it asks
/// for nothing: the session says `changed` and Java draws again.
fn redraws(event: &UiEvent) -> bool {
    matches!(
        event,
        UiEvent::Engine(_)
            | UiEvent::SendDone(_)
            | UiEvent::SendUnproven(_)
            | UiEvent::FetchDone(_)
            | UiEvent::Open { .. }
            | UiEvent::Close
            | UiEvent::Write { .. }
            | UiEvent::React { .. }
            // The home's answer lists a new group, or adds members,
            // with no effect to run (1307 pass five): the list is
            // drawn again, or the group stays invisible until a copy
            // arrives.
            | UiEvent::GroupMade(_)
            | UiEvent::MembersAdded(_)
            // What the hub said, or a verb of the Rooms tab (1384): the
            // pane is drawn again.
            | UiEvent::Rooms(_)
            // What the fetch brought (1629, walk 3: a page that came was
            // drawn only after the next tap, and "lade" stood 30 s under
            // it). Not the tick, which comes every turn, nor a node heard
            // (1491, 1595: no list moves for it).
            | UiEvent::Browse(
                BrowseEvent::Loaded { .. }
                    | BrowseEvent::Failed(_)
                    | BrowseEvent::FailedText(_)
                    | BrowseEvent::Downloaded { .. }
                    | BrowseEvent::DownloadFailed(_)
                    | BrowseEvent::Identifying { .. }
            )
    )
}

/// A join that failed, in one plain sentence for the join screen (1263,
/// finding 8): no daemon, no server, no TCP on the main path.
fn join_words(error: &loquax::home::client::HomeError) -> StrId {
    use loquax::home::client::HomeError;
    match error {
        HomeError::NoPath => StrId::JoinNoAnswer,
        HomeError::Refused(_) => StrId::JoinRefused,
        HomeError::Timeout => StrId::JoinSlow,
        _ => StrId::JoinFailed,
    }
}

/// The same join failure in the words of the machinery, for the screen's
/// Details: what someone helping needs.
fn join_details(error: &loquax::home::client::HomeError, server: &str) -> String {
    use loquax::home::client::HomeError;
    match error {
        HomeError::NoPath => format!(
            "No path to the mailbox through {server}: check the address, \
             that its lnsd runs a TCP server there, and that this phone can reach it."
        ),
        HomeError::Refused(status) => format!("The mailbox refused the code: {status}."),
        HomeError::Timeout => format!("No answer through {server} in time."),
        other => format!("Joining failed: {other}."),
    }
}

/// Whether the leaf's interface to the home node is up: its only one, the
/// TCP client, which is down from the moment the server goes until it
/// redials.
/// The idle pump's sleep: until the pull schedule's next pull when there
/// is one, never longer than [`IDLE_PUMP`], never shorter than [`PUMP`].
fn idle_wait(until_sync: Option<Duration>) -> Duration {
    until_sync.unwrap_or(IDLE_PUMP).clamp(PUMP, IDLE_PUMP)
}

/// Hand the watching link's word on to the session and wake its pump;
/// ends with either side.
async fn relay_watch(
    mut heard: tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    session: tokio::sync::mpsc::UnboundedSender<WatchEvent>,
    wake: Arc<tokio::sync::Notify>,
) {
    loop {
        tokio::select! {
            event = heard.recv() => {
                let Some(event) = event else { return };
                if session.send(event).is_err() {
                    return;
                }
                wake.notify_one();
            }
            () = session.closed() => return,
        }
    }
}

/// What the node says that the pump must hear while it may sleep (1346):
/// an interface that went down or lost its carrier wakes it, so "not
/// connected" comes within
/// [`NOT_CONNECTED_WITHIN`] and not at the idle pump's next turn, and each
/// link to the home this phone opened goes to the `WAKE_SUMMARY` count.
/// Ends with the node's events or the session's end of `opened`.
async fn relay_node(
    mut tap: leviculum_std::driver::EventTap,
    home: leviculum_core::DestinationHash,
    opened: tokio::sync::mpsc::UnboundedSender<leviculum_core::LinkId>,
    wake: Arc<tokio::sync::Notify>,
) {
    use leviculum_core::node::NodeEvent;
    use leviculum_std::driver::TapEvent;
    // A TCP client that lost its connection redials on its own and tells
    // the core nothing: its carrier says it.
    let mut carrier = leviculum_std::interfaces::carrier_changes();
    loop {
        tokio::select! {
            Ok(()) = carrier.changed() => wake.notify_one(),
            event = tap.recv() => match event {
                Some(TapEvent::Event(NodeEvent::InterfaceDown(_))) | Some(TapEvent::Lagged(_)) => {
                    wake.notify_one();
                }
                Some(TapEvent::Event(NodeEvent::LinkEstablished {
                    link_id,
                    is_initiator: true,
                    destination_hash,
                })) if destination_hash == home => {
                    if opened.send(link_id).is_err() {
                        return;
                    }
                }
                Some(_) => {}
                None => return,
            },
            () = opened.closed() => return,
        }
    }
}

/// The mailbox's word on itself (1642) as the phone holds it.
#[derive(Default)]
struct AboutAsk {
    told: loquax::ui::mailbox_page::Told,
    asking: bool,
    /// When the last ask began, on the pump's clock.
    began: Option<u64>,
}

fn lock_about(about: &Mutex<AboutAsk>) -> MutexGuard<'_, AboutAsk> {
    about
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The mailbox page of `model` (1642, `loquax::ui::mailbox_page`) in
/// `lang`'s words, as [`schema::MailboxPage`] carries it to the settings
/// page: the scene the window and the terminal draw, resolved.
pub fn mailbox_page_words<B>(model: &UiModel<B>, lang: Lang) -> schema::MailboxPage {
    let page = loquax::ui::mailbox_page::page(model);
    schema::MailboxPage {
        v: VERSION,
        title: page.title.resolve(lang),
        groups: page
            .groups
            .iter()
            .map(|group| schema::MailboxGroup {
                title: group.title.resolve(lang),
                lines: group
                    .lines
                    .iter()
                    .map(|line| schema::MailboxLine {
                        label: line.label.resolve(lang),
                        value: line.value.as_ref().map(|v| v.resolve(lang)),
                        copy: line.copy.clone(),
                        action: line.action.as_ref().map(|action| {
                            match action.intent {
                                loquax::ui::intent::Intent::Profile => "profile",
                                _ => "",
                            }
                            .to_string()
                        }),
                        note: line.note,
                    })
                    .collect(),
            })
            .collect(),
        copy_label: StrId::MailboxCopy.text(lang).to_string(),
        copied: StrId::MailboxCopied.text(lang).to_string(),
    }
}

fn link_up(node: &leviculum_std::driver::ReticulumNode) -> bool {
    node.interface_stats()
        .iter()
        .any(|interface| !interface.is_local_client && interface.online)
}

/// The core's profile scene of `model` (1443, `loquax::ui::profile`) in
/// `lang`'s words, as [`schema::Profile::words`] carries it to the
/// activity: the scene the window and the terminal draw, resolved.
pub fn profile_words<B>(model: &UiModel<B>, lang: Lang) -> schema::ProfileWords {
    let screen = loquax::ui::profile::profile(model);
    schema::ProfileWords {
        title: screen.title.resolve(lang),
        name: screen.name.resolve(lang),
        address_label: screen.address_label.resolve(lang),
        fours: screen.fours.clone(),
        caption: screen.caption.resolve(lang),
        mailbox: screen.mailbox.resolve(lang),
        state: screen
            .state
            .as_ref()
            .map(|state| state.resolve(lang))
            .unwrap_or_default(),
        devices_label: screen.devices_label.resolve(lang),
        devices: screen.devices.iter().map(|d| d.resolve(lang)).collect(),
        share: screen.share.label_key.text(lang).to_string(),
    }
}

/// An address in groups of four hex digits, to read out and compare.
pub fn fingerprint(peer: &[u8; 16]) -> String {
    let hex = loquax::address::to_hex(peer);
    hex.as_bytes()
        .chunks(4)
        .map(|group| String::from_utf8_lossy(group).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The host of a `host:port` server, an IPv6 host without its brackets.
fn server_host(server: &str) -> &str {
    let host = server.rsplit_once(':').map_or(server, |(host, _)| host);
    host.trim_start_matches('[').trim_end_matches(']')
}

/// `home`'s address book table, stamped with when this device began on
/// its first start (1448, walk finding 12): the banner asks only for a
/// person met after it. A stamp that cannot be written asks nothing this
/// run and is tried again on the next start.
fn began(home: &Path) -> Book {
    let mut book = links::load(home);
    if book.since.is_none() {
        book.since = Some(leviculum_std::local_time::unix_now_secs());
        if let Err(error) = links::save(home, &book) {
            tracing::warn!("{}: {error}", links::BOOK_FILE);
        }
    }
    book
}

fn wall_now(offset: i32) -> WallTime {
    WallTime {
        unix_secs: leviculum_std::local_time::unix_now_secs(),
        offset_secs: offset,
    }
}

/// The kept list, one `host:port` a line; a file from before the list holds
/// one line and reads as a list of one.
fn load_servers(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join(SERVER_FILE))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn save_servers(home: &Path, servers: &[String]) -> Result<(), String> {
    let mut text = servers.join("\n");
    text.push('\n');
    loquax::home::client::write_atomically(&home.join(SERVER_FILE), &text)
}

/// What the leaf dials, in order (1196): a server given beside the code
/// wins, as a fixed address; then the config's `home_address`,
/// `rendezvous_dial` and `rendezvous` keys; then the list the code carried,
/// else the one kept from the last start; then the shipped rendezvous list,
/// unless this host holds the shipped defaults back
/// ([`loquax::home::rendezvous::defaults_held_back`], a rig or CI host).
/// `None` when nothing is left to dial: the join screen asks for an address.
fn dial_list(
    given: Option<&str>,
    config: &loquax::config::Config,
    carried: Vec<String>,
    home: &Path,
) -> Option<Vec<String>> {
    use loquax::home::rendezvous::{default_list, defaults_held_back, reach, Reach};
    // A code's servers are the code's business (1471): a field that only
    // shows the code's tail, the whole list `mailbox admit` printed, is not
    // a server the user typed, and the join walks the code's ladder in its
    // order. A server typed by hand is one `host:port` and wins.
    if let Some(given) = given.map(str::trim).filter(|given| !given.is_empty()) {
        if !is_carried(given, &carried) {
            return Some(vec![given.to_string()]);
        }
    }
    let known = match carried.is_empty() {
        true => load_servers(home),
        false => carried,
    };
    match reach(config, &known) {
        Reach::Fixed(fixed) => Some(vec![fixed]),
        Reach::MeshOnly => None,
        Reach::List(list) if list == default_list() && defaults_held_back() => None,
        Reach::List(list) => Some(list),
    }
}

/// Whether `given` is the list `carried` a code brought, as a field shows
/// it: its entries joined by the code's separator, whitespace ignored.
fn is_carried(given: &str, carried: &[String]) -> bool {
    let compact: String = given.chars().filter(|c| !c.is_whitespace()).collect();
    !carried.is_empty() && compact == carried.join(&loquax::home::code::LIST_SEPARATOR.to_string())
}

/// One server the user typed, `host:port`, or why it is not one. A list,
/// which only a code carries (1471), is named as such: the parser would
/// read its commas as part of the host and blame an IPv6 bracket.
fn one_server(text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.contains(loquax::home::code::LIST_SEPARATOR) {
        return Err(format!(
            "'{text}' names several servers: write one, as host:port, e.g. 192.168.1.20:4242"
        ));
    }
    loquax::engine::parse_server(text).map(|_| ())
}

/// A peer's address as the app hands it back.
pub fn parse_peer(text: &str) -> Result<[u8; 16], String> {
    loquax::address::parse(text.trim())
        .map_err(|error| format!("'{}' is not an address: {error}", text.trim()))
}

/// A message id in hex.
pub fn parse_id(text: &str) -> Result<[u8; 32], String> {
    let text = text.trim();
    loquax::address::parse_id(text).ok_or_else(|| format!("'{text}' is not a message id"))
}

/// The Advanced page's public rooms' hub (1577) for the phone: the
/// core's rows ([`loquax::ui::settings::hub_rows`]) in `lang`, each line
/// with the word Java sends back to choose it (`none`, or the hub's
/// destination, which is never shown).
pub fn rooms_hub_choice<B>(
    model: &loquax::ui::UiModel<B>,
    lang: Lang,
    error: String,
) -> schema::RoomsHubChoice {
    use loquax::ui::intent::Intent;
    use loquax::ui::rooms::HubPick;
    let mut choice = schema::RoomsHubChoice {
        v: VERSION,
        ok: error.is_empty(),
        error,
        ..schema::RoomsHubChoice::default()
    };
    for row in loquax::ui::settings::hub_rows(model, loquax::ui::settings::Target::Phone) {
        let label = row.label.resolve(lang);
        let value = row.value.as_ref().map(|v| v.resolve(lang));
        match row.action.map(|a| a.intent) {
            Some(Intent::RoomsHub(HubPick::Hub(hub))) => {
                // The core marks the hub in force with a check at the
                // value's end; Java draws its own.
                let value = value.unwrap_or_default();
                let detail = value
                    .trim_end_matches('\u{2713}')
                    .trim_end_matches(loquax::ui::strings::JOIN)
                    .trim();
                choice.hubs.push(schema::RoomsHubRow {
                    name: label,
                    chosen: value.ends_with('\u{2713}'),
                    set: hub.iter().map(|b| format!("{b:02x}")).collect(),
                    detail: detail.to_string(),
                })
            }
            Some(Intent::RoomsHub(HubPick::More)) => choice.hubs.push(schema::RoomsHubRow {
                name: label,
                chosen: false,
                set: "more".to_string(),
                detail: String::new(),
            }),
            Some(Intent::RoomsHub(HubPick::None)) => choice.hubs.push(schema::RoomsHubRow {
                name: label,
                chosen: value.is_some(),
                set: "none".to_string(),
                detail: String::new(),
            }),
            Some(Intent::RoomsHub(HubPick::Paste)) => {
                choice.paste = label;
                choice.own = value.unwrap_or_default();
            }
            Some(_) => {}
            None if choice.title.is_empty() => {
                choice.title = label;
                choice.current = value.unwrap_or_default();
            }
            None => choice.note = label,
        }
    }
    choice.hint = Text::key(StrId::RoomsHubPasteHint).resolve(lang);
    choice
}

/// The settings' sections as the phone lists them (1450): the core's one
/// list for the phone (`loquax::ui::settings::entries`), in `lang`.
pub fn settings_list(lang: Lang) -> schema::SettingsList {
    use loquax::ui::settings::{entries, Target};
    schema::SettingsList {
        v: VERSION,
        sections: entries(Target::Phone)
            .iter()
            .map(|entry| schema::SettingsSection {
                key: entry.section.key().to_string(),
                title: entry.title.resolve(lang),
                summary: entry.summary.resolve(lang),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {

    /// 1629 (walk 3 new 1 and 2): what the fetch brings marks the session
    /// changed, so the Browse pane draws the page when it comes, not at the
    /// next tap; the tick, every turn, does not.
    #[test]
    fn a_page_the_fetch_brought_redraws_and_the_tick_does_not() {
        use super::*;
        let page = UiEvent::Browse(BrowseEvent::Loaded {
            micron: "page".into(),
            title: "page".into(),
        });
        assert!(redraws(&page));
        assert!(redraws(&UiEvent::Browse(BrowseEvent::Failed(
            "no path".into()
        ))));
        assert!(!redraws(&UiEvent::Browse(BrowseEvent::Tick)));
        assert!(!redraws(&UiEvent::Browse(BrowseEvent::Node(
            lnomad::DiscoveredNode {
                dest_hash: [1; 16],
                name: None,
                first_seen: 0,
                last_seen: 0,
                hops: None,
            }
        ))));
    }

    /// 1450 (walk 1, finding 21): the phone's settings list is the core's
    /// one list, the nine sections in their order, in the phone's
    /// language, each by the key `SettingsActivity` opens its page by.
    #[test]
    fn the_settings_list_is_the_cores_nine_in_their_order() {
        use loquax::ui::settings::SECTIONS;
        let list = settings_list(Lang::De);
        let keys: Vec<&str> = list.sections.iter().map(|s| s.key.as_str()).collect();
        let want: Vec<&str> = SECTIONS.iter().map(|s| s.key()).collect();
        assert_eq!(keys, want);
        let titles: Vec<&str> = list.sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Profil",
                "Postfach",
                "Darstellung",
                "Chats",
                "Benachrichtigungen",
                "Privatsphäre",
                "Karten",
                "Sicherung",
                "Erweitert"
            ]
        );
        assert!(list.sections.iter().all(|s| !s.summary.is_empty()));
    }

    /// 1577: the Advanced page's hub lines are the core's, in the phone's
    /// language: the line that says which, the shipped hubs and none with
    /// the mark on the one in force and the word that chooses each, the
    /// paste line and what a hub learns. An own hub is named by its words,
    /// never by its hash.
    #[test]
    fn the_hub_lines_are_the_cores_in_the_phones_language() {
        use loquax::ui::rooms::{HubChoice, RoomsEvent};
        let mut model = UiModel::<Line>::new(&[]);
        let choice = rooms_hub_choice(&model, Lang::De, String::new());
        assert!(choice.ok);
        assert_eq!(choice.title, "Server für öffentliche Räume");
        assert_eq!(choice.current, "Keiner");
        // 1599: the closest ten of the shipped list, "More…", then none.
        let names: Vec<&str> = choice.hubs.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names.len(), 12, "{names:?}");
        assert_eq!(names[0], "RNS Moscow");
        assert_eq!((names[10], names[11]), ("Mehr…", "Keiner"));
        assert_eq!(choice.hubs[0].set, "42a97b1b07147b898f78a610dfbba587");
        // Never heard here: no distance (1629, walk 3 new 5).
        assert_eq!(choice.hubs[0].detail, "");
        assert_eq!(choice.hubs[10].set, "more");
        assert_eq!(choice.hubs[11].set, "none");
        assert!(choice.hubs[11].chosen && !choice.hubs[0].chosen);
        // "More…": the list scrolls through every hub, and no More.
        update(&mut model, UiEvent::Rooms(RoomsEvent::More));
        let all = rooms_hub_choice(&model, Lang::De, String::new());
        assert_eq!(all.hubs.len(), loquax::ui::hubs::shipped().len() + 1);
        assert!(all.hubs.iter().all(|h| h.set != "more"));
        assert_eq!(choice.paste, "Anderer Server: Adresse einfügen");
        // No name on this phone: the room shows none (1629, walk 3 new 9).
        assert_eq!(
            choice.note,
            "Du hast hier keinen Namen: Im Raum erscheinst du ohne Namen."
        );
        model.profile.name = "Lew".into();
        let named = rooms_hub_choice(&model, Lang::De, String::new());
        assert_eq!(named.note, "Dein Name ist im Raum für alle sichtbar.");
        assert!(choice.own.is_empty());
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Hub(HubChoice::Hub([0x07; 16]))),
        );
        let choice = rooms_hub_choice(&model, Lang::De, String::new());
        assert_eq!(choice.current, "Eigener Server");
        assert_eq!(choice.own, "Eigener Server");
        assert!(choice.hubs.iter().all(|h| !h.chosen));
        assert!(!schema::encode(&choice).is_empty());
    }
    use super::*;

    /// 1471: a code with three servers joins over all three, in the code's
    /// order, whether the field is empty or shows the code's tail (what the
    /// join screen's field held on the Pixel, refused before 1471). A server
    /// typed by hand wins; a hand-typed list is refused with words that say
    /// it is a list, under the sentence naming one address.
    #[test]
    fn a_code_with_several_servers_dials_them_all_in_order() {
        let home = tempfile::tempdir().expect("a dir");
        let config = loquax::config::Config::default();
        let three = vec![
            "192.168.178.61:4343".to_string(),
            "leviculum.network:4343".to_string(),
            "leviculum.network:4242".to_string(),
        ];
        let field = "192.168.178.61:4343,leviculum.network:4343,leviculum.network:4242";
        assert_eq!(
            dial_list(None, &config, three.clone(), home.path()),
            Some(three.clone())
        );
        assert_eq!(
            dial_list(Some(field), &config, three.clone(), home.path()),
            Some(three.clone())
        );
        assert_eq!(
            dial_list(
                Some(" 192.168.178.61:4343, leviculum.network:4343,\nleviculum.network:4242 "),
                &config,
                three.clone(),
                home.path()
            ),
            Some(three.clone())
        );
        // One server in the code.
        let one = vec!["127.0.0.1:4242".to_string()];
        assert_eq!(
            dial_list(Some("127.0.0.1:4242"), &config, one.clone(), home.path()),
            Some(one)
        );
        // A server typed by hand, with or without servers in the code.
        assert_eq!(
            dial_list(Some("home.example:4242"), &config, three, home.path()),
            Some(vec!["home.example:4242".to_string()])
        );
        assert_eq!(
            dial_list(Some("home.example:4242"), &config, Vec::new(), home.path()),
            Some(vec!["home.example:4242".to_string()])
        );
        // A list typed by hand, not the code's, stays the user's and is refused.
        let typed = "a.example:4242,b.example:4242";
        assert_eq!(
            dial_list(Some(typed), &config, Vec::new(), home.path()),
            Some(vec![typed.to_string()])
        );
        let refused = one_server(typed).expect_err("a list is not one server");
        assert!(refused.contains("several servers"), "{refused}");
        assert!(one_server("home.example:4242").is_ok());
        // Control: without the list check the parser blames a bracket.
        assert!(loquax::engine::parse_server(typed)
            .expect_err("the parser refuses it too")
            .contains("IPv6"));
    }

    /// 1307 pass five: the home's answer to Create or Add asks for no
    /// effect, and without a `changed` the list Java shows never takes the
    /// new group's row until a copy arrives. Control: a tick changes no
    /// screen by itself.
    #[test]
    fn the_homes_answer_on_a_group_draws_the_list_again() {
        use loquax::ui::groups::NotMade;
        assert!(redraws(&UiEvent::GroupMade(Ok([1; 16]))));
        assert!(redraws(&UiEvent::GroupMade(Err(NotMade::HomeTooOld))));
        assert!(redraws(&UiEvent::MembersAdded(Ok([1; 16]))));
        assert!(!redraws(&UiEvent::Tick));
    }

    /// Every join failure is one plain sentence of the catalogue in both
    /// languages (1263, finding 8), and the machinery's words, the server
    /// included, go under Details only.
    #[test]
    fn a_join_failure_is_one_plain_sentence_and_the_rest_is_details() {
        use loquax::home::client::HomeError;
        use loquax::home::wire::Status;
        let server = "192.168.1.20:4242";
        let cases = [
            (
                HomeError::NoPath,
                "The mailbox did not answer. Is the computer on?",
            ),
            (
                HomeError::Refused(Status::BadToken),
                "The mailbox did not take this code. Make a new one on the computer.",
            ),
            (
                HomeError::Timeout,
                "The mailbox did not answer in time. Try again.",
            ),
            (
                HomeError::NoLink("refused".to_string()),
                "Joining the mailbox did not work.",
            ),
            (HomeError::Malformed, "Joining the mailbox did not work."),
        ];
        for (error, english) in cases {
            assert_eq!(join_words(&error).text(Lang::En), english, "{error:?}");
            for lang in Lang::ALL {
                let said = join_words(&error).text(lang);
                for word in ["lnsd", "TCP", "host:port", "loquax home", server] {
                    assert!(!said.contains(word), "{lang:?} {error:?}: {said}");
                }
            }
            assert!(!join_details(&error, server).is_empty());
        }
        assert!(join_details(&HomeError::NoPath, server).contains(server));
        assert!(join_details(&HomeError::NoPath, server).contains("lnsd"));
        assert_eq!(
            join_words(&HomeError::NoPath).text(Lang::De),
            "Das Postfach hat nicht geantwortet. Ist der Computer an?"
        );
    }

    /// The client's own periodic timers while it idles (1176), as the
    /// report lists them. Tokio exposes no list of the timers a runtime
    /// holds, so this pins the periods instead: the pump's idle sleep and
    /// the watch's backstop are both the link keepalive's ceiling, and the
    /// idle sleep ends early only for the pull schedule.
    /// 1346: a mailbox that goes silent is "not connected" within the
    /// bound the phone promises, not after the old 26 s and not after an
    /// idle pump's sleep: the kernel drops the long-idle connection after
    /// its idle and one probe, and the `InterfaceDown` wakes the pump,
    /// which looks within [`LINK_EVERY_MS`].
    #[test]
    fn a_silent_mailbox_is_not_connected_within_the_promised_bound() {
        let section = |tcp: loquax::config::TcpLiveness| leviculum_std::config::InterfaceConfig {
            tcp_keepalive_idle_secs: tcp.idle_secs,
            tcp_keepalive_interval_secs: tcp.interval_secs,
            tcp_keepalive_probes: tcp.probes,
            tcp_user_timeout_secs: tcp.user_timeout_secs,
            ..Default::default()
        };
        let reference = section(loquax::config::TcpLiveness::default())
            .tcp_silent_peer_bound()
            .expect("in range");
        let device = section(loquax::config::TcpLiveness::default().for_device())
            .tcp_silent_peer_bound()
            .expect("in range");
        assert_eq!(reference, Duration::from_secs(26), "before 1346");
        assert_eq!(device, Duration::from_secs(242));
        assert!(device + Duration::from_millis(LINK_EVERY_MS) <= NOT_CONNECTED_WITHIN);
        assert!(
            NOT_CONNECTED_WITHIN < IDLE_PUMP,
            "the promise does not rest on the idle pump's turn"
        );
    }

    /// The window closes once an hour of the pump's clock, with every
    /// counter in the line, and starts over.
    #[test]
    fn the_wake_summary_is_said_once_an_hour() {
        let mut costs = DeviceCosts::default();
        costs.window.pumps = 12;
        costs.window.links = 1;
        assert_eq!(costs.summary(3_599_999), None);
        assert_eq!(
            costs.summary(3_600_000).as_deref(),
            Some("WAKE_SUMMARY keepalives=0 links=1 pumps=12 bytes=0")
        );
        assert_eq!(costs.window, loquax::home::keepalive::Costs::default());
        assert_eq!(costs.summary(7_100_000), None);
        assert!(costs.summary(7_200_000).is_some());
    }

    #[test]
    fn an_idle_client_wakes_no_more_often_than_the_link_keepalive() {
        let keepalive = Duration::from_secs(leviculum_core::constants::LINK_KEEPALIVE_SECS);
        assert_eq!(IDLE_PUMP, keepalive);
        assert_eq!(watch::LOOK, keepalive);
        assert_eq!(idle_wait(None), keepalive, "no pull scheduled");
        assert_eq!(
            idle_wait(Some(Duration::from_secs(900))),
            keepalive,
            "a pull later than the ceiling"
        );
        assert_eq!(
            idle_wait(Some(Duration::from_secs(30))),
            Duration::from_secs(30),
            "the pull schedule's next pull ends the sleep"
        );
        assert_eq!(idle_wait(Some(Duration::ZERO)), PUMP, "never a busy loop");
    }
}
