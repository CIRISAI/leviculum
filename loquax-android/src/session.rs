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
use loquax::engine::{attach_leaf, Attached, LeafConfig, Role};
use loquax::fanout::Fanout;
use loquax::home::runner::{self, HomeClient};
use loquax::home::watch::{self, Fallback, WatchEvent};
use loquax::outbox::{Command, Outbox, OutboxEvent};
use loquax::store::Store;
use loquax::ui::intent::{Intent, Messages, Switch, Target};
use loquax::ui::liveness::Liveness;
use loquax::ui::map::MapPin;
use loquax::ui::recording::{Effect, Haptic, Input, LevelTrack, Verb};
use loquax::ui::strings::{Lang, StrId, Text};
use loquax::ui::{chat, compose, scene, update, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;

use crate::links::{self, Book};
use crate::mailbox;
use crate::pull_line::{PullLine, Say};
use crate::schema::{self, Event, VERSION};

/// The file under the app's directory that keeps the home node's address.
pub const SERVER_FILE: &str = "home-server";
/// How long a join, a send or a mark may take; a pull gets twice this.
pub const BUDGET: Duration = Duration::from_secs(60);
/// How often the session takes what the engine said and runs the clock
/// while something is going on: the terminal's pump interval.
pub const PUMP: Duration = Duration::from_millis(100);
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
/// How long a send runner waits for its bubble to be named.
const SETTLE: Duration = Duration::from_secs(5);
/// How often the TCP interface is looked at while the pump runs fast.
const LINK_EVERY_MS: u64 = 1_000;
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
    Pull(runner::PullEnd),
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
    sending: bool,
    pull_owed: bool,
    pulling: bool,
    /// Whether the user asked for the pull owed (Refresh), and what the
    /// status line is told of the running one (1136).
    pull_asked: bool,
    pull_line: PullLine,
    finished_tx: Sender<Finished>,
    finished_rx: Receiver<Finished>,
    events: Sender<Event>,
    runtime: tokio::runtime::Handle,
    origin: std::time::Instant,
    /// Something the screen shows changed since the last `changed`.
    dirty: bool,
    gone: bool,
    /// Whether the TCP interface to the home node was up at the last look,
    /// none before the first, and when that look was.
    link: Option<bool>,
    link_looked: Option<u64>,
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
    /// The peers added by a code while this session runs (walk finding
    /// 15): their chat does not ask to save them to the phone's contacts,
    /// the user just named them.
    just_added: std::collections::BTreeSet<[u8; 16]>,
    /// The contact keys the home node took (1187), for the facade test.
    keys_told: Arc<std::sync::atomic::AtomicUsize>,
    /// What the recorder is to do (1276, `UiEffect::Recording`), by peer,
    /// until [`schema::Query::Recording`] hands it out.
    recorder: Vec<([u8; 16], Effect)>,
}

/// A running session.
pub struct Session {
    inner: Arc<Mutex<Inner>>,
    /// Wakes the pump when a request may have started work (1176).
    wake: Arc<tokio::sync::Notify>,
    events: Mutex<Receiver<Event>>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    /// The mailbox, when it is on this phone.
    here: Mutex<Option<mailbox::Here>>,
    /// The device "Link a device" waits for (1264), when one was asked.
    linking: Mutex<Option<mailbox::Linking>>,
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
        let (start, here) = if start.here || mailbox::is_here(&home) {
            let (start, here) = Self::start_here(start, &home, runtime.handle())?;
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
            loquax::engine::parse_server(server).map_err(|text| {
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
            },
            Role::Client { address },
        ));
        let attached = attached.map_err(|error| StartError::other(error.to_string()))?;
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
        let inner = Arc::new(Mutex::new(inner));
        let pumped = Arc::clone(&inner);
        let pump_wake = Arc::clone(&wake);
        runtime.spawn(async move {
            let mut wait = PUMP;
            loop {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = pump_wake.notified() => {}
                }
                let Ok(mut inner) = pumped.lock() else {
                    return;
                };
                if !inner.pump() {
                    return;
                }
                wait = inner.next_pump();
            }
        });
        let session = Self {
            inner,
            wake,
            events: Mutex::new(events_rx),
            runtime: Mutex::new(Some(runtime)),
            here: Mutex::new(here),
            linking: Mutex::new(None),
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
    ) -> Result<(schema::Start, mailbox::Here), StartError> {
        if let Some(line) = start.restore.as_deref() {
            mailbox::restore(home, line, start.passphrase.as_deref().unwrap_or_default()).map_err(
                |text| StartError {
                    need: Some("passphrase"),
                    text,
                    details: None,
                },
            )?;
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
        let here = mailbox::Here::start(home, runtime).map_err(StartError::other)?;
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
        Ok(schema::Profile {
            v: VERSION,
            name: if here {
                mailbox::name(&home)
            } else {
                String::new()
            },
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
        })
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

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic under the lock is caught at the JNI edge; what it left
        // is still the best state there is.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// What the user did. The pump is woken after it: whatever the request
    /// started (a send, a pull, a screen in front) runs on the pump's turn,
    /// and an idle pump would otherwise sleep through it (1176).
    pub fn request(&self, request: schema::Request) -> Result<(), String> {
        let done = self.requested(request);
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
            Intent::Front(front) => {
                let mut inner = self.lock();
                inner.front = front;
                inner.apply(UiEvent::Front(front));
                return Ok(());
            }
            Intent::Viewport { peer, keys } => UiEvent::Viewport { peer, keys },
            Intent::Name(name) => return self.rename(&name),
            Intent::AddContact { lxma, name } => return self.add_contact(&lxma, name.as_deref()),
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
            schema::Query::LinkDevice => return self.link_device().map(|c| schema::encode(&c)),
            schema::Query::Linked => return self.linked().map(|l| schema::encode(&l)),
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
        let inner = self.lock();
        Ok(match query {
            schema::Query::Conversations { search } => {
                schema::encode(&inner.conversations(search.as_deref()))
            }
            schema::Query::Messages { peer } => schema::encode(&inner.messages(parse_peer(&peer)?)),
            schema::Query::Bar { peer } => schema::encode(&inner.bar(parse_peer(&peer)?)),
            schema::Query::Shares => schema::encode(&inner.shares()),
            schema::Query::Info { peer, key } => {
                let peer = parse_peer(&peer)?;
                let detail = loquax::ui::message_detail(&inner.model, peer, schema::model_key(key))
                    .ok_or_else(|| "no such message".to_string())?;
                schema::encode(&schema::Info {
                    v: VERSION,
                    rows: detail.rows,
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
                        name: inner.model.names.label(peer),
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
            | schema::Query::LinkDevice
            | schema::Query::Linked
            | schema::Query::Backup { .. }
            | schema::Query::MapRegions { .. }
            | schema::Query::MapTheme { .. }
            | schema::Query::MapFling { .. }
            | schema::Query::Receipts { .. }
            | schema::Query::PhotoSize { .. }
            | schema::Query::Photo { .. }
            | schema::Query::Staged { .. }
            | schema::Query::SaveFile { .. }
            | schema::Query::Recording { .. }
            | schema::Query::Voice { .. } => return Err("answered above".to_string()),
            // The viewer's zoom needs no session: `lib.rs` answers it.
            schema::Query::Zoom(ask) => return Ok(crate::viewer::answer(&ask)),
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
        model.home_client = true;
        model.list_apart = true;
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
            sending: false,
            pull_owed: false,
            pulling: false,
            pull_asked: false,
            pull_line: PullLine::default(),
            finished_tx,
            finished_rx,
            events,
            runtime,
            origin: std::time::Instant::now(),
            dirty: true,
            gone: false,
            link: None,
            // The first pump looks, and says what it saw.
            link_looked: None,
            wake,
            watch,
            watching: false,
            front: false,
            fallback: Fallback::default(),
            book: links::load(home),
            just_added: std::collections::BTreeSet::new(),
            keys_told: Arc::default(),
            recorder: Vec::new(),
        })
    }

    /// One turn of the pump: what the engine said, what the runners
    /// finished, the clock. `false` once the session is over.
    fn pump(&mut self) -> bool {
        if self.gone {
            return false;
        }
        match self.fanout.pump(&mut self.attached.outbox) {
            Ok(events) => {
                for event in events {
                    if runner::already_held(&self.store, &event) {
                        continue;
                    }
                    runner::keep_signature(&mut self.store, &event);
                    let named = match &event {
                        OutboxEvent::Announced {
                            destination,
                            name: Some(_),
                        } => Some(*destination),
                        _ => None,
                    };
                    self.apply(UiEvent::Engine(event));
                    if let Some(peer) = named {
                        runner::keep_name(
                            &mut self.store,
                            &self.model.names,
                            self.model.address,
                            peer,
                        );
                    }
                }
            }
            Err(gone) => {
                self.gone = true;
                let _ = self.events.send(Event::Stopped {
                    v: VERSION,
                    detail: gone.to_string(),
                });
                return false;
            }
        }
        while let Ok(finished) = self.finished_rx.try_recv() {
            match finished {
                Finished::Send(event) => {
                    self.sending = false;
                    self.apply(event);
                }
                Finished::Pull(end) => self.pulled(end),
            }
        }
        self.look_at_link();
        let elapsed = self.elapsed_ms();
        if let Some(say) = self.pull_line.tick(elapsed) {
            self.say(say);
        }
        self.watched(elapsed);
        // Applied, not quiet: a running share sends from the clock (1273).
        self.apply(UiEvent::Clock(wall_now(self.offset)));
        self.quiet(UiEvent::Tick);
        self.apply(UiEvent::Elapsed(elapsed));
        if std::mem::take(&mut self.dirty) {
            let _ = self.events.send(Event::Changed { v: VERSION });
        }
        true
    }

    /// How long the pump may sleep before its next turn (1176): [`PUMP`]
    /// while anything is going on, else until the pull schedule's next
    /// pull, at most [`IDLE_PUMP`].
    fn next_pump(&self) -> Duration {
        let busy = self.front
            || !self.watching
            || self.sending
            || self.pulling
            || self.pull_owed
            || !self.sends.is_empty()
            || self.link.is_none()
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
        if self
            .link_looked
            .is_some_and(|at| now.saturating_sub(at) < LINK_EVERY_MS)
        {
            return;
        }
        self.link_looked = Some(now);
        let up = link_up(&self.client.node);
        if up && self.link == Some(false) {
            self.ask_for_home_path();
        }
        if self.link != Some(up) {
            self.link = Some(up);
            let _ = self.events.send(Event::Link { v: VERSION, up });
        }
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

    /// Tell the status line about a pull.
    fn say(&self, say: Say) {
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
        let shown = matches!(
            event,
            UiEvent::Engine(_)
                | UiEvent::SendDone(_)
                | UiEvent::SendUnproven(_)
                | UiEvent::FetchDone(_)
                | UiEvent::Open { .. }
                | UiEvent::Close
                | UiEvent::Write { .. }
                | UiEvent::React { .. }
        );
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
        for effect in effects {
            match effect {
                UiEffect::Send { to, body, fields } => self.sends.push_back((to, body, fields)),
                UiEffect::Fetch => self.pull_owed = true,
                UiEffect::Log(line) | UiEffect::Notice(line) => tracing::info!("{line}"),
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
                // What the terminal does with its screen and its maps has
                // no place here yet.
                _ => {}
            }
        }
        self.start_next();
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
        let petnames = self.model.take_petnames();
        if !petnames.is_empty() {
            self.runtime
                .spawn(runner::hand_petnames(self.client.clone(), petnames, BUDGET));
        }
        let states = self.model.take_peer_states();
        if !states.is_empty() {
            self.runtime.spawn(runner::hand_peer_states(
                self.client.clone(),
                states,
                BUDGET,
            ));
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
        self.dirty = true;
        Ok(())
    }

    fn unlink(&mut self, peer: [u8; 16]) -> Result<(), String> {
        if self.book.links.remove(&peer).is_none() {
            return Ok(());
        }
        links::save(&self.home, &self.book)?;
        self.model.names.unlink(&peer);
        self.dirty = true;
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
                let _ = done.send(Finished::Pull(end));
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
    fn pulled(&mut self, end: runner::PullEnd) {
        self.pulling = false;
        runner::settle_pull(&self.home, end.cursor);
        if end.imported > 0 {
            self.persist();
            match runner::show_imported(&mut self.model, &self.store) {
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
        self.apply(UiEvent::FetchDone(end.result));
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
        let rows = found(&self.model, order, search)
            .into_iter()
            .map(|row| self.schema_row(row))
            .collect();
        let (address, initials) = self.model.address.map_or_else(Default::default, |own| {
            (loquax::address::to_hex(&own), self.initials(&own))
        });
        schema::Conversations {
            v: VERSION,
            conversations: rows,
            address,
            initials,
            archived: 0,
            home_client: self.model.home_client,
            home_link: self.watching,
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
        let row = row.row;
        schema::ConversationRow {
            peer: loquax::address::to_hex(&row.peer),
            name: row.name,
            last: row.last,
            ours: row.ours,
            at: row.at.map_or(0, |at| at.unix_secs),
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
        let heard = index.is_some_and(|c| {
            self.model.conversations[c]
                .messages
                .iter()
                .any(|m| !m.outbound)
        });
        let names = &self.model.names;
        let liveness = self.model.liveness(&peer);
        // The field opens with the scene's compose (1219).
        let compose = index.map(|c| scene::compose(&self.model, c));
        let bar = self.bar(peer);
        schema::Messages {
            v: VERSION,
            peer: loquax::address::to_hex(&peer),
            name: self.model.names.label(&peer),
            messages: rows,
            initials: self.initials(&peer),
            icon_fg: icon.as_ref().map(|(fg, _)| fg.clone()),
            icon_bg: icon.map(|(_, bg)| bg),
            home_client: self.model.home_client,
            photo: self.photo(&peer),
            linked: link.is_some(),
            lookup: link.map(|link| link.lookup.clone()),
            contact_id: link.map(|link| link.contact_id),
            ask_link: link.is_none()
                && heard
                && !self.book.not_now.contains(&peer)
                && !self.just_added.contains(&peer),
            announced: names.announced(&peer).map(str::to_string),
            petname: names.petname(&peer).map(str::to_string),
            others: names
                .same_contact(&peer)
                .iter()
                .map(|other| names.label(other))
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

    /// A linked peer's copied thumbnail.
    fn photo(&self, peer: &[u8; 16]) -> Option<String> {
        self.book
            .links
            .contains_key(peer)
            .then(|| links::photo(&self.home, peer))
            .flatten()
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
    model.names = loquax::names::Names::new(contacts);
    for (peer, name) in store.names().map_err(|error| error.to_string())? {
        model.names.remember(peer, &name);
    }
    for (peer, link) in links::load(home).links {
        model.names.link(peer, &link.name, &link.lookup);
    }
    model.restore_positions(store.positions().map_err(|error| error.to_string())?);
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
            let mut row = row(node.bubble);
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
        R::Viewport { peer, keys } => Intent::Viewport {
            peer: parse_peer(&peer)?,
            keys: keys.into_iter().map(schema::model_key).collect(),
        },
        R::Name { name } => Intent::Name(name),
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

fn link_up(node: &leviculum_std::driver::ReticulumNode) -> bool {
    node.interface_stats()
        .iter()
        .any(|interface| !interface.is_local_client && interface.online)
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
    if let Some(given) = given.map(str::trim).filter(|given| !given.is_empty()) {
        return Some(vec![given.to_string()]);
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

#[cfg(test)]
mod tests {
    use super::*;

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
