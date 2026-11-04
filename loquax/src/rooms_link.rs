//! The Rooms' hub link (1379, #232): Loquax as a Reticulum Relay Chat
//! client, over the node the messenger already runs, lent by its engine
//! ([`crate::engine::Lender::Rooms`]) as the Browse tab's fetch is, so the
//! phone and the desktop carry it alike.
//!
//! One hub at a time: a link to its `rrc.hub` destination, identified
//! with the reader's identity (the hub takes the link's identity as the
//! sender), HELLO until WELCOME, then joins, parts and lines as plain link
//! packets, one envelope each ([`leviculum_rrc::Session`] knows what to
//! send and what the hub's answers mean). Nothing here is stored or
//! retried: a room is live, a line a hub never saw is gone, and the
//! frontends say so when a room is opened.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use leviculum_core::Identity;
use leviculum_rrc::{Event, Refusal, Session};
use leviculum_std::{DestinationHash, LinkId, NodeEvent, ReticulumNode};
use rand_core::RngCore;
use tokio::sync::{mpsc, Notify};

use crate::engine::Lent;

/// How long a path to the hub is looked for.
const PATH_BUDGET: Duration = Duration::from_secs(30);
const PATH_RETRY: Duration = Duration::from_secs(2);
/// How long the link may take to come up.
const LINK_BUDGET: Duration = Duration::from_secs(30);
/// HELLO is sent this often until WELCOME comes, at most this many times.
const HELLO_EVERY: Duration = Duration::from_secs(5);
const HELLO_TRIES: u32 = 5;

/// The name and version the client gives in its HELLO.
pub const CLIENT_NAME: &str = "loquax";

/// What the frontend asks of the hub link: the model's own words.
pub use crate::ui::rooms::{Down, RoomAsk as Ask};

/// What the hub link tells the frontend: the model's own events.
pub use crate::ui::rooms::RoomsEvent as Told;

/// The ends the frontend holds.
pub struct Linking {
    pub asks: mpsc::UnboundedSender<Ask>,
    pub told: mpsc::UnboundedReceiver<Told>,
}

/// What the hub link needs: where the lent node comes from, the identity
/// the link is identified with, and the nick shown in the rooms.
pub struct Options {
    pub lends: mpsc::UnboundedReceiver<Lent>,
    pub identity: Identity,
    pub nick: Option<String>,
}

/// Start the hub link on `runtime`; `wake` is told whenever there is
/// something to take.
pub fn start(runtime: &tokio::runtime::Handle, options: Options, wake: Arc<Notify>) -> Linking {
    let (asks, asks_rx) = mpsc::unbounded_channel();
    let (told_tx, told) = mpsc::unbounded_channel();
    runtime.spawn(run(
        options,
        asks_rx,
        Teller {
            told: told_tx,
            wake,
        },
    ));
    Linking { asks, told }
}

struct Teller {
    told: mpsc::UnboundedSender<Told>,
    wake: Arc<Notify>,
}

impl Teller {
    fn tell(&self, told: Told) {
        if self.told.send(told).is_ok() {
            self.wake.notify_one();
        }
    }
}

/// The hub we are linked to.
struct Hub {
    link_id: LinkId,
    session: Session,
    hellos: u32,
    next_hello: tokio::time::Instant,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_id() -> [u8; 8] {
    let mut id = [0u8; 8];
    rand_core::OsRng.fill_bytes(&mut id);
    id
}

struct Task {
    lent: Option<Lent>,
    hub: Option<Hub>,
    identity: Identity,
    nick: Option<String>,
    teller: Teller,
}

async fn run(mut options: Options, mut asks: mpsc::UnboundedReceiver<Ask>, teller: Teller) {
    let mut task = Task {
        lent: None,
        hub: None,
        identity: options.identity,
        nick: options.nick,
        teller,
    };
    let mut lends_open = true;
    loop {
        let hello_due = task
            .hub
            .as_ref()
            .filter(|hub| !hub.session.is_welcomed())
            .map(|hub| hub.next_hello);
        // Biased, the lend first: the shell lends the node and asks the
        // first Open before this task runs, and an Open taken before its
        // lend finds no node.
        tokio::select! {
            biased;
            lend = options.lends.recv(), if lends_open => match lend {
                Some(lend) => {
                    if task.hub.take().is_some() {
                        task.teller.tell(Told::Down(Down::Lost));
                    }
                    task.lent = Some(lend);
                }
                None => lends_open = false,
            },
            ask = asks.recv() => match ask {
                Some(ask) => task.ask(ask).await,
                None => {
                    task.close().await;
                    return;
                }
            },
            event = next_event(&mut task.lent) => match event {
                Some(event) => task.event(event).await,
                None => {
                    task.lent = None;
                    if task.hub.take().is_some() {
                        task.teller.tell(Told::Down(Down::NodeGone));
                    }
                }
            },
            () = sleep_until(hello_due) => task.hello_again().await,
        }
    }
}

/// The next event of the lent node, or never while nothing is lent.
async fn next_event(lent: &mut Option<Lent>) -> Option<NodeEvent> {
    match lent {
        Some(lent) => lent.events.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl Task {
    fn node(&self) -> Option<Arc<ReticulumNode>> {
        self.lent.as_ref().and_then(|lent| lent.node.upgrade())
    }

    async fn send(&self, link_id: &LinkId, bytes: &[u8]) -> bool {
        match self.node() {
            Some(node) => node.send_link_packet(link_id, bytes).await.is_ok(),
            None => false,
        }
    }

    async fn ask(&mut self, ask: Ask) {
        match ask {
            Ask::Open { hub } => self.open(hub).await,
            Ask::Close => {
                self.close().await;
                self.teller.tell(Told::Down(Down::Left));
            }
            Ask::Join { room, key } => {
                self.out(|s| s.join(&room, key.as_deref(), fresh_id(), now_ms()))
                    .await;
            }
            Ask::Part { room } => self.out(|s| s.part(&room, fresh_id(), now_ms())).await,
            Ask::Say { room, kind, text } => {
                self.out(|s| s.say(&room, kind, &text, fresh_id(), now_ms()))
                    .await;
            }
        }
    }

    /// Send what `make` makes of the session, or tell why not.
    async fn out(&mut self, make: impl FnOnce(&mut Session) -> Result<Vec<u8>, Refusal>) {
        let Some(hub) = self.hub.as_mut() else {
            self.teller.tell(Told::Refused(Refusal::NotReady));
            return;
        };
        match make(&mut hub.session) {
            Ok(bytes) => {
                let link_id = hub.link_id;
                if !self.send(&link_id, &bytes).await {
                    self.hub = None;
                    self.teller.tell(Told::Down(Down::Lost));
                }
            }
            Err(refusal) => self.teller.tell(Told::Refused(refusal)),
        }
    }

    async fn close(&mut self) {
        if let (Some(hub), Some(node)) = (self.hub.take(), self.node()) {
            let _ = node.close_link(&hub.link_id).await;
        }
    }

    async fn open(&mut self, dest: [u8; 16]) {
        self.close().await;
        self.teller.tell(Told::Linking);
        let Some(node) = self.node() else {
            self.teller.tell(Told::Down(Down::NodeGone));
            return;
        };
        let hash = DestinationHash::new(dest);
        if !node.has_path(&hash)
            && !node
                .wait_for_path(&hash, PATH_BUDGET, PATH_RETRY)
                .await
                .unwrap_or(false)
        {
            self.teller.tell(Told::Down(Down::NoPath));
            return;
        }
        let Some(identity) = node.get_identity(&hash) else {
            self.teller.tell(Told::Down(Down::NoPath));
            return;
        };
        let public = identity.public_key_bytes();
        let mut signing = [0u8; 32];
        signing.copy_from_slice(&public[32..64]);
        let link_id = match node.connect(&hash, &signing).await {
            Ok(handle) => *handle.link_id(),
            Err(_) => {
                self.teller.tell(Told::Down(Down::NoAnswer));
                return;
            }
        };
        drop(node);
        if !self.established(link_id).await {
            if let Some(node) = self.node() {
                let _ = node.close_link(&link_id).await;
            }
            self.teller.tell(Told::Down(Down::NoAnswer));
            return;
        }
        let Some(node) = self.node() else {
            self.teller.tell(Told::Down(Down::NodeGone));
            return;
        };
        if node.identify_link(&link_id, &self.identity).await.is_err() {
            let _ = node.close_link(&link_id).await;
            self.teller.tell(Told::Down(Down::NoAnswer));
            return;
        }
        let mut session = Session::new(
            *self.identity.hash(),
            self.nick.clone(),
            CLIENT_NAME,
            crate::VERSION,
        );
        if let Some(mdu) = node.link_mdu(&link_id) {
            session.set_mdu(mdu);
        }
        drop(node);
        self.hub = Some(Hub {
            link_id,
            session,
            hellos: 0,
            next_hello: tokio::time::Instant::now(),
        });
        self.hello_again().await;
    }

    /// Wait for our link to come up; other events meanwhile are dropped,
    /// as nothing else is open on this lend.
    async fn established(&mut self, link_id: LinkId) -> bool {
        let Some(lent) = self.lent.as_mut() else {
            return false;
        };
        let wait = async {
            while let Some(event) = lent.events.recv().await {
                match event {
                    NodeEvent::LinkEstablished { link_id: id, .. } if id == link_id => return true,
                    NodeEvent::LinkClosed { link_id: id, .. } if id == link_id => return false,
                    _ => {}
                }
            }
            false
        };
        tokio::time::timeout(LINK_BUDGET, wait)
            .await
            .unwrap_or(false)
    }

    async fn hello_again(&mut self) {
        let Some(hub) = self.hub.as_mut() else {
            return;
        };
        if hub.hellos >= HELLO_TRIES {
            self.close().await;
            self.teller.tell(Told::Down(Down::NoAnswer));
            return;
        }
        hub.hellos += 1;
        hub.next_hello = tokio::time::Instant::now() + HELLO_EVERY;
        let bytes = hub.session.hello(fresh_id(), now_ms());
        let link_id = hub.link_id;
        if !self.send(&link_id, &bytes).await {
            self.hub = None;
            self.teller.tell(Told::Down(Down::Lost));
        }
    }

    async fn event(&mut self, event: NodeEvent) {
        let Some(hub) = self.hub.as_mut() else {
            return;
        };
        let ours = hub.link_id;
        match event {
            NodeEvent::LinkDataReceived { link_id, data } if link_id == ours => {
                let Ok(received) = hub.session.receive(&data, fresh_id(), now_ms()) else {
                    // Not an envelope: a hub of another protocol version,
                    // or noise. Nothing to show.
                    return;
                };
                if let Some(told) = told(received.event) {
                    self.teller.tell(told);
                }
                if let Some(reply) = received.reply {
                    let _ = self.send(&ours, &reply).await;
                }
            }
            NodeEvent::LinkClosed { link_id, .. } if link_id == ours => {
                self.hub = None;
                self.teller.tell(Told::Down(Down::Lost));
            }
            _ => {}
        }
    }
}

/// What one of the hub's envelopes tells the Rooms; a PING answered, a
/// PONG and what a client has no use for tell nothing.
fn told(event: Event) -> Option<Told> {
    Some(match event {
        Event::Welcome { hub, limits, .. } => Told::Welcomed {
            name: hub,
            body_limit: limits.body,
        },
        Event::Joined { room, nick } => Told::Joined { room, nick },
        Event::Parted { room, nick } => Told::Parted { room, nick },
        Event::Line {
            room,
            kind,
            text,
            nick,
            ours,
            ts,
            ..
        } => Told::Line {
            room,
            kind,
            text,
            nick,
            ours,
            ts_ms: ts,
        },
        Event::HubNotice { room, text } => Told::HubNotice { room, text },
        Event::Error { room, text } => Told::Error { room, text },
        Event::Pong | Event::Pinged | Event::Ignored(_) => return None,
    })
}

/// The Rooms' half of a desktop shell (1379): the hub link over the
/// attached node, lent to [`crate::engine::Lender::Rooms`], or nothing
/// while the config names no hub.
pub struct Rooming {
    linking: Option<Linking>,
}

impl Rooming {
    /// No hub: no tab, nothing lent.
    pub fn off() -> Self {
        Rooming { linking: None }
    }

    /// The hub link over `attached`'s node, identified with `identity`,
    /// showing `nick`.
    pub fn start(
        attached: &crate::engine::Attached,
        identity: Identity,
        nick: Option<String>,
    ) -> Self {
        let (lend, lends) = mpsc::unbounded_channel();
        let _ = lend.send(attached.lend_to(crate::engine::Lender::Rooms));
        let linking = start(
            &tokio::runtime::Handle::current(),
            Options {
                lends,
                identity,
                nick,
            },
            // The shell reads the link on its own pump; nobody waits on
            // the wake.
            Arc::new(Notify::new()),
        );
        Rooming {
            linking: Some(linking),
        }
    }

    /// One of the Rooms' asks, handed to the link.
    pub fn ask(&self, ask: Ask) {
        if let Some(linking) = &self.linking {
            let _ = linking.asks.send(ask);
        }
    }

    /// What the link said since the last time, as the model's events.
    pub fn told(&mut self) -> Vec<crate::ui::UiEvent> {
        let mut out = Vec::new();
        if let Some(linking) = &mut self.linking {
            while let Ok(told) = linking.told.try_recv() {
                out.push(crate::ui::UiEvent::Rooms(told));
            }
        }
        out
    }
}
