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
/// How long the link may take to come up: the rooms screen's patience
/// (1659, walk 10, 53: at 30 s the screen gave up on a server the sheet
/// had seen answer after 45 s).
const LINK_BUDGET: Duration = Duration::from_secs(45);
/// HELLO is sent this often until WELCOME comes, at most this many times.
const HELLO_EVERY: Duration = Duration::from_secs(5);
const HELLO_TRIES: u32 = 5;
/// How long a server's notices after its WELCOME are read for the rooms
/// they name (1621), as the probe that measured the shipped list reads them.
pub const NOTICE_WINDOW: Duration = Duration::from_secs(6);
/// The most a question to a server may take, path, link and HELLOs
/// ([`crate::ui::rooms::PEEK_BUDGET_SECS`], which the sheet waits for).
const PEEK_BUDGET: Duration =
    Duration::from_secs(crate::ui::rooms::PEEK_BUDGET_SECS.unsigned_abs());
/// How long the answer to `/list` is waited for after the WELCOME (1652);
/// a hub that says nothing in time names what its greeting named.
pub const LIST_WINDOW: Duration = Duration::from_secs(15);

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
            hub: None,
        },
    ));
    Linking { asks, told }
}

struct Teller {
    told: mpsc::UnboundedSender<Told>,
    wake: Arc<Notify>,
    /// The hub the rooms' link is for, for the event log (1652).
    hub: Option<[u8; 16]>,
}

impl Teller {
    fn tell(&self, told: Told) {
        if let Some(hub) = &self.hub {
            let state = match &told {
                Told::Linking => Some("linking"),
                Told::Welcomed { .. } => Some("ready"),
                Told::Down(Down::NoPath) => Some("no_path"),
                Told::Down(Down::NoAnswer) => Some("no_answer"),
                Told::Down(Down::Lost) => Some("lost"),
                Told::Down(Down::NodeGone) => Some("node_gone"),
                Told::Down(Down::Left) => Some("left"),
                _ => None,
            };
            if let Some(state) = state {
                crate::events::rooms_link(hub, state);
            }
        }
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
    /// The rooms its answer to `/list` named (1652), once it came.
    listed: Option<Vec<String>>,
    deadline: tokio::time::Instant,
}

/// The hub we are linked to.
struct Hub {
    hash: [u8; 16],
    link_id: LinkId,
    session: Session,
    hellos: u32,
    next_hello: tokio::time::Instant,
    /// Until when the answer to `/list` is waited for (1652), while it is.
    listing: Option<tokio::time::Instant>,
    /// The hub's room-less notices since its WELCOME: its greeting, read
    /// for `#word` hits beside the list.
    greeting: String,
    /// The joins sent that the hub has not answered yet, for the log.
    joining: Vec<String>,
}

impl Hub {
    /// The hub's clock: a HELLO before its WELCOME, the end of the list's
    /// window after.
    fn due(&self) -> Option<tokio::time::Instant> {
        match self.session.is_welcomed() {
            false => Some(self.next_hello),
            true => self.listing,
        }
    }
}

/// What a hub named (1652): its `/list` answer (`None` when none came in
/// time) merged with its greeting's hits.
fn named(listed: Option<&[String]>, greeting: &str) -> Vec<String> {
    let hits = crate::ui::hubs::rooms_named(greeting);
    crate::ui::hubs::rooms_merged(listed.unwrap_or(&[]), &hits)
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
        let hub_due = task.hub.as_ref().and_then(Hub::due);
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
                        task.peek_done(None, "node_gone").await;
                    }
                }
            },
            link = links.recv() => {
                if let Some((hub, link_id)) = link {
                    task.peek_linked(hub, link_id).await;
                }
            }
            () = sleep_until(hub_due) => task.hub_due().await,
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
                let sent = self
                    .out(|s| s.join(&room, key.as_deref(), fresh_id(), now_ms()))
                    .await;
                match (sent, self.hub.as_mut()) {
                    (true, Some(hub)) => {
                        let name = leviculum_rrc::room_name(&room, usize::MAX).unwrap_or(room);
                        if hub.joining.len() < 16 && !hub.joining.contains(&name) {
                            hub.joining.push(name);
                        }
                    }
                    _ => crate::events::rooms_join(&room, false),
                }
            }
            Ask::Part { room } => {
                self.out(|s| s.part(&room, fresh_id(), now_ms())).await;
            }
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
                listed: None,
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
            None => self.peek_done(None, "no_link").await,
        }
    }

    /// The question's link is up: identified with its throwaway identity,
    /// then HELLO.
    async fn peek_up(&mut self) {
        let Some(node) = self.node() else {
            return self.peek_done(None, "node_gone").await;
        };
        let Some(peek) = self.peek.as_mut() else {
            return;
        };
        let Some(link_id) = peek.link_id else {
            return;
        };
        if node.identify_link(&link_id, &peek.identity).await.is_err() {
            return self.peek_done(None, "identify_failed").await;
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
            return self.peek_done(None, "no_welcome").await;
        }
        peek.hellos += 1;
        peek.next = tokio::time::Instant::now() + HELLO_EVERY;
        let bytes = session.hello(fresh_id(), now_ms());
        if !self.send(&link_id, &bytes).await {
            self.peek_done(None, "send_failed").await;
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
            let named = named(peek.listed.as_deref(), &peek.notices);
            return self.peek_done(Some(named), "no_list").await;
        }
        if now >= peek.deadline {
            let named = peek
                .welcomed
                .then(|| named(peek.listed.as_deref(), &peek.notices));
            return self.peek_done(named, "timeout").await;
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
                let mut list = None;
                let mut answered = false;
                match received.event {
                    Event::Welcome { .. } if !peek.welcomed => {
                        peek.welcomed = true;
                        peek.next = tokio::time::Instant::now() + NOTICE_WINDOW;
                        // The hub's own list (1652), beside its greeting.
                        list = session
                            .command(crate::ui::hubs::LIST_COMMAND, fresh_id(), now_ms())
                            .ok();
                    }
                    Event::HubNotice { room: None, text } if peek.listed.is_none() => {
                        match crate::ui::hubs::list_answer(&text) {
                            Some(rooms) => {
                                peek.listed = Some(rooms);
                                answered = true;
                            }
                            None => {
                                peek.notices.push_str(&text);
                                peek.notices.push('\n');
                            }
                        }
                    }
                    // A hub without the command: its greeting is all.
                    Event::Error { room: None, .. } if peek.welcomed && peek.listed.is_none() => {
                        peek.listed = Some(Vec::new());
                        answered = true;
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
                if let Some(list) = list {
                    let _ = self.send(&link_id, &list).await;
                }
                if answered {
                    if let Some(peek) = self.peek.as_ref() {
                        // The greeting came with the WELCOME, before the
                        // list's answer: all there is is here.
                        let named = named(peek.listed.as_deref(), &peek.notices);
                        return self.peek_done(Some(named), "-").await;
                    }
                }
            }
            NodeEvent::LinkClosed { .. } => {
                let named = self.peek.as_ref().and_then(|peek| {
                    peek.welcomed
                        .then(|| named(peek.listed.as_deref(), &peek.notices))
                });
                if let Some(peek) = self.peek.as_mut() {
                    // Closed under us: nothing left to close.
                    peek.link_id = None;
                }
                self.peek_done(named, "link_closed").await;
            }
            _ => {}
        }
    }

    /// The question answered (`Some`, maybe no rooms) or not: told, its
    /// link closed, the next one asked.
    ///
    /// Every end leaves its line (1669, walk 12, finding 68: a sheet gave
    /// up with no line at all): `why` is how a question ended that brought
    /// no list, `timeout` its budget over, `-` when the hub's list came.
    async fn peek_done(&mut self, named: Option<Vec<String>>, why: &str) {
        let Some(peek) = self.peek.take() else {
            return;
        };
        self.early.clear();
        if let (Some(link_id), Some(node)) = (peek.link_id, self.node()) {
            let _ = node.close_link(&link_id).await;
        }
        let answered = named.is_some() && peek.listed.is_some();
        crate::events::rooms_list(
            &peek.hub,
            named.as_ref().map_or(0, Vec::len),
            "peek",
            answered,
            if answered { "-" } else { why },
        );
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

    /// Send what `make` makes of the session, or tell why not; whether it
    /// went.
    async fn out(&mut self, make: impl FnOnce(&mut Session) -> Result<Vec<u8>, Refusal>) -> bool {
        let Some(hub) = self.hub.as_mut() else {
            self.teller.tell(Told::Refused(Refusal::NotReady));
            return false;
        };
        match make(&mut hub.session) {
            Ok(bytes) => {
                let link_id = hub.link_id;
                if !self.send(&link_id, &bytes).await {
                    self.hub = None;
                    self.teller.tell(Told::Down(Down::Lost));
                    return false;
                }
                true
            }
            Err(refusal) => {
                self.teller.tell(Told::Refused(refusal));
                false
            }
        }
    }

    async fn close(&mut self) {
        if let (Some(hub), Some(node)) = (self.hub.take(), self.node()) {
            let _ = node.close_link(&hub.link_id).await;
        }
    }

    async fn open(&mut self, dest: [u8; 16]) {
        self.close().await;
        self.teller.hub = Some(dest);
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
            hash: dest,
            link_id,
            session,
            hellos: 0,
            next_hello: tokio::time::Instant::now(),
            listing: None,
            greeting: String::new(),
            joining: Vec::new(),
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

    /// The hub's clock ([`Hub::due`]): a HELLO again, or the list's
    /// window over without an answer.
    async fn hub_due(&mut self) {
        let Some(hub) = self.hub.as_mut() else {
            return;
        };
        if !hub.session.is_welcomed() {
            return self.hello_again().await;
        }
        if hub
            .listing
            .is_some_and(|until| tokio::time::Instant::now() >= until)
        {
            self.listed(None);
        }
    }

    /// The list's answer came (`Some`) or its window ended: what the hub
    /// named is told, once (1652).
    fn listed(&mut self, answer: Option<Vec<String>>) {
        let Some(hub) = self.hub.as_mut() else {
            return;
        };
        if hub.listing.take().is_none() {
            return;
        }
        let rooms = named(answer.as_deref(), &hub.greeting);
        let why = if answer.is_some() { "-" } else { "timeout" };
        crate::events::rooms_list(&hub.hash, rooms.len(), "link", answer.is_some(), why);
        self.teller.tell(Told::Listed { rooms });
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
                let mut list = None;
                let mut answer = None;
                let mut quiet = false;
                match &received.event {
                    Event::Welcome { version, .. } => {
                        crate::events::rooms_welcome(&hub.hash, version.as_deref());
                        // Its rooms, without a name typed (1652): asked once
                        // a link, answered with a NOTICE.
                        list = hub
                            .session
                            .command(crate::ui::hubs::LIST_COMMAND, fresh_id(), now_ms())
                            .ok();
                        hub.listing = list
                            .is_some()
                            .then(|| tokio::time::Instant::now() + LIST_WINDOW);
                        hub.greeting.clear();
                    }
                    Event::HubNotice { room: None, text } if hub.listing.is_some() => {
                        match crate::ui::hubs::list_answer(text) {
                            Some(rooms) => {
                                answer = Some(rooms);
                                quiet = true;
                            }
                            None => {
                                hub.greeting.push_str(text);
                                hub.greeting.push('\n');
                            }
                        }
                    }
                    // A hub without `/list` refuses it: no toast for that,
                    // its greeting is what it names.
                    Event::Error { room: None, .. } if hub.listing.is_some() => {
                        answer = Some(Vec::new());
                        quiet = true;
                    }
                    Event::Joined { room, nick: None } => {
                        hub.joining.retain(|r| r != room);
                        crate::events::rooms_join(room, true);
                    }
                    Event::Error {
                        room: Some(room), ..
                    } if hub.joining.contains(room) => {
                        hub.joining.retain(|r| r != room);
                        crate::events::rooms_join(room, false);
                    }
                    _ => {}
                }
                if !quiet {
                    if let Some(told) = told(received.event) {
                        self.teller.tell(told);
                    }
                }
                if let Some(reply) = received.reply {
                    let _ = self.send(&ours, &reply).await;
                }
                if let Some(list) = list {
                    let _ = self.send(&ours, &list).await;
                }
                if let Some(answer) = answer {
                    self.listed(Some(answer));
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
