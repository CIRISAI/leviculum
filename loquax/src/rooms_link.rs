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
/// How long a server's notices after its WELCOME are read for the rooms
/// they name (1621), as the probe that measured the shipped list reads them.
pub const NOTICE_WINDOW: Duration = Duration::from_secs(6);
/// The most a question to a server may take, path, link and HELLOs.
const PEEK_BUDGET: Duration = Duration::from_secs(90);

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

/// A question to a server of the rooms it names (1621, [`Ask::Peek`]): a
/// link of its own, identified with a throwaway identity and no nick, so
/// the server learns nothing of the reader; it joins nothing.
struct Peek {
    hub: [u8; 16],
    /// The link, once asked for; the session once it is up.
    link_id: Option<LinkId>,
    session: Option<Session>,
    identity: Identity,
    hellos: u32,
    /// The next HELLO, or the end of the notices' window once welcomed.
    next: tokio::time::Instant,
    welcomed: bool,
    notices: String,
    deadline: tokio::time::Instant,
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
    /// The question to a server on its way, one at a time, and those
    /// waiting behind it.
    peek: Option<Peek>,
    peeks: std::collections::VecDeque<[u8; 16]>,
    /// Where the question's path and link come back from their own task.
    linked: mpsc::UnboundedSender<([u8; 16], Option<LinkId>)>,
    /// Events of the question's link that came while an open waited for
    /// its own link.
    missed: Vec<NodeEvent>,
    /// Links that came up while the question's link id was still on its
    /// way back from its task: a link may come up before the task's word.
    early: Vec<LinkId>,
}

async fn run(mut options: Options, mut asks: mpsc::UnboundedReceiver<Ask>, teller: Teller) {
    let (linked, mut links) = mpsc::unbounded_channel();
    let mut task = Task {
        lent: None,
        hub: None,
        identity: options.identity,
        nick: options.nick,
        teller,
        peek: None,
        peeks: std::collections::VecDeque::new(),
        linked,
        missed: Vec::new(),
        early: Vec::new(),
    };
    let mut lends_open = true;
    loop {
        let hello_due = task
            .hub
            .as_ref()
            .filter(|hub| !hub.session.is_welcomed())
            .map(|hub| hub.next_hello);
        let peek_due = task.peek.as_ref().map(|peek| match peek.session {
            Some(_) => peek.next.min(peek.deadline),
            None => peek.deadline,
        });
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
                    if task.peek.is_some() {
                        task.peek_done(None).await;
                    }
                }
            },
            link = links.recv() => {
                if let Some((hub, link_id)) = link {
                    task.peek_linked(hub, link_id).await;
                }
            }
            () = sleep_until(hello_due) => task.hello_again().await,
            () = sleep_until(peek_due) => task.peek_due().await,
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
            Ask::Peek { hub } => {
                if self.peek.as_ref().is_some_and(|p| p.hub == hub) || self.peeks.contains(&hub) {
                    return;
                }
                self.peeks.push_back(hub);
                if self.peek.is_none() {
                    self.peek_next().await;
                }
            }
        }
    }

    /// The next question waiting, its path and link looked for on a task
    /// of their own so the rooms' link is read meanwhile.
    async fn peek_next(&mut self) {
        while let Some(hub) = self.peeks.pop_front() {
            let Some(node) = self.node() else {
                self.teller.tell(Told::Peeked { hub, rooms: None });
                continue;
            };
            let now = tokio::time::Instant::now();
            self.peek = Some(Peek {
                hub,
                link_id: None,
                session: None,
                identity: Identity::generate(&mut rand_core::OsRng),
                hellos: 0,
                next: now,
                welcomed: false,
                notices: String::new(),
                deadline: now + PEEK_BUDGET,
            });
            let linked = self.linked.clone();
            tokio::spawn(async move {
                let hash = DestinationHash::new(hub);
                let reached = node.has_path(&hash)
                    || node
                        .wait_for_path(&hash, PATH_BUDGET, PATH_RETRY)
                        .await
                        .unwrap_or(false);
                let link_id = match (reached, node.get_identity(&hash)) {
                    (true, Some(identity)) => {
                        let public = identity.public_key_bytes();
                        let mut signing = [0u8; 32];
                        signing.copy_from_slice(&public[32..64]);
                        node.connect(&hash, &signing)
                            .await
                            .ok()
                            .map(|handle| *handle.link_id())
                    }
                    _ => None,
                };
                let _ = linked.send((hub, link_id));
            });
            return;
        }
    }

    /// The question's link was asked for, or there is none to ask.
    async fn peek_linked(&mut self, hub: [u8; 16], link_id: Option<LinkId>) {
        let Some(peek) = self.peek.as_mut().filter(|p| p.hub == hub) else {
            // A question given up meanwhile: its link goes.
            if let (Some(link_id), Some(node)) = (link_id, self.node()) {
                let _ = node.close_link(&link_id).await;
            }
            return;
        };
        match link_id {
            Some(link_id) => {
                peek.link_id = Some(link_id);
                if core::mem::take(&mut self.early).contains(&link_id) {
                    self.peek_up().await;
                }
            }
            None => self.peek_done(None).await,
        }
    }

    /// The question's link is up: identified with its throwaway identity,
    /// then HELLO.
    async fn peek_up(&mut self) {
        let Some(node) = self.node() else {
            return self.peek_done(None).await;
        };
        let Some(peek) = self.peek.as_mut() else {
            return;
        };
        let Some(link_id) = peek.link_id else {
            return;
        };
        if node.identify_link(&link_id, &peek.identity).await.is_err() {
            return self.peek_done(None).await;
        }
        let mut session = Session::new(*peek.identity.hash(), None, CLIENT_NAME, crate::VERSION);
        if let Some(mdu) = node.link_mdu(&link_id) {
            session.set_mdu(mdu);
        }
        drop(node);
        peek.session = Some(session);
        self.peek_hello().await;
    }

    async fn peek_hello(&mut self) {
        let Some(peek) = self.peek.as_mut() else {
            return;
        };
        let (Some(session), Some(link_id)) = (peek.session.as_mut(), peek.link_id) else {
            return;
        };
        if peek.hellos >= HELLO_TRIES {
            return self.peek_done(None).await;
        }
        peek.hellos += 1;
        peek.next = tokio::time::Instant::now() + HELLO_EVERY;
        let bytes = session.hello(fresh_id(), now_ms());
        if !self.send(&link_id, &bytes).await {
            self.peek_done(None).await;
        }
    }

    /// The question's clock: the notices' window is over, a HELLO is due,
    /// or the question took too long.
    async fn peek_due(&mut self) {
        let Some(peek) = self.peek.as_ref() else {
            return;
        };
        let now = tokio::time::Instant::now();
        if peek.welcomed && now >= peek.next {
            let named = crate::ui::hubs::rooms_named(&peek.notices);
            return self.peek_done(Some(named)).await;
        }
        if now >= peek.deadline {
            let named = peek
                .welcomed
                .then(|| crate::ui::hubs::rooms_named(&peek.notices));
            return self.peek_done(named).await;
        }
        if peek.session.is_some() && !peek.welcomed && now >= peek.next {
            self.peek_hello().await;
        }
    }

    /// One of the question link's events.
    async fn peek_event(&mut self, event: NodeEvent) {
        match event {
            NodeEvent::LinkEstablished { .. } => self.peek_up().await,
            NodeEvent::LinkDataReceived { link_id, data } => {
                let Some(peek) = self.peek.as_mut() else {
                    return;
                };
                let Some(session) = peek.session.as_mut() else {
                    return;
                };
                let Ok(received) = session.receive(&data, fresh_id(), now_ms()) else {
                    return;
                };
                match received.event {
                    Event::Welcome { .. } if !peek.welcomed => {
                        peek.welcomed = true;
                        peek.next = tokio::time::Instant::now() + NOTICE_WINDOW;
                    }
                    Event::HubNotice { text, .. } => {
                        peek.notices.push_str(&text);
                        peek.notices.push('\n');
                    }
                    _ => {}
                }
                if let Some(reply) = received.reply {
                    let _ = self.send(&link_id, &reply).await;
                }
            }
            NodeEvent::LinkClosed { .. } => {
                let named = self.peek.as_ref().and_then(|peek| {
                    peek.welcomed
                        .then(|| crate::ui::hubs::rooms_named(&peek.notices))
                });
                if let Some(peek) = self.peek.as_mut() {
                    // Closed under us: nothing left to close.
                    peek.link_id = None;
                }
                self.peek_done(named).await;
            }
            _ => {}
        }
    }

    /// The question answered (`Some`, maybe no rooms) or not: told, its
    /// link closed, the next one asked.
    async fn peek_done(&mut self, named: Option<Vec<String>>) {
        let Some(peek) = self.peek.take() else {
            return;
        };
        self.early.clear();
        if let (Some(link_id), Some(node)) = (peek.link_id, self.node()) {
            let _ = node.close_link(&link_id).await;
        }
        self.teller.tell(Told::Peeked {
            hub: peek.hub,
            rooms: named,
        });
        Box::pin(self.peek_next()).await;
    }

    /// Whether `event` is the question link's.
    fn peeks_at(&self, event: &NodeEvent) -> bool {
        let Some(ours) = self.peek.as_ref().and_then(|p| p.link_id) else {
            return false;
        };
        match event {
            NodeEvent::LinkEstablished { link_id, .. }
            | NodeEvent::LinkDataReceived { link_id, .. }
            | NodeEvent::LinkClosed { link_id, .. } => *link_id == ours,
            _ => false,
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
        let up = self.established(link_id).await;
        for event in core::mem::take(&mut self.missed) {
            Box::pin(self.peek_event(event)).await;
        }
        if !up {
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
        let peek = self.peek.as_ref().and_then(|p| p.link_id);
        let Some(lent) = self.lent.as_mut() else {
            return false;
        };
        let missed = &mut self.missed;
        let wait = async {
            while let Some(event) = lent.events.recv().await {
                match event {
                    NodeEvent::LinkEstablished { link_id: id, .. } if id == link_id => return true,
                    NodeEvent::LinkClosed { link_id: id, .. } if id == link_id => return false,
                    // A server's question meanwhile keeps its events (1621).
                    NodeEvent::LinkEstablished { link_id: id, .. }
                    | NodeEvent::LinkDataReceived { link_id: id, .. }
                    | NodeEvent::LinkClosed { link_id: id, .. }
                        if Some(id) == peek =>
                    {
                        missed.push(event);
                    }
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
        if self.peeks_at(&event) {
            return self.peek_event(event).await;
        }
        if let NodeEvent::LinkEstablished { link_id, .. } = &event {
            if self.peek.as_ref().is_some_and(|p| p.link_id.is_none()) && self.early.len() < 8 {
                self.early.push(*link_id);
            }
        }
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
