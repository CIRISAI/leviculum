//! The Rooms' hub link end to end (1379, #232): Loquax's link task over a
//! lent node, through a shared-instance daemon, to a hub on `rrc.hub`.
//!
//! The hub is the fake of `support/rrc_hub.rs`, written from the RRC
//! specification with the envelopes recorded from the reference hub.
//!
//! ```text
//! rooms_link ── lent node ── Unix socket ── daemon ── TCP ── fake hub
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use leviculum_rrc::{Envelope, Kind, LineKind, Refusal};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::ReticulumNode;
use loquax::engine::Lent;
use loquax::rooms_link::{self, Ask, Down, Told};
use tokio::sync::Notify;

#[path = "support/rrc_hub.rs"]
mod rrc_hub;
use rrc_hub::{vector, Hub};

/// The host: a node of its own on the shared instance whose events it
/// forwards to the lend, as Loquax's processor tap does.
type Host = (
    Arc<ReticulumNode>,
    Lent,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Sender<()>,
);

async fn host(instance: &str, storage: &tempfile::TempDir) -> Host {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(instance)
        .storage_path(storage.path().to_path_buf())
        .build_sync()
        .expect("build the host");
    let mut events = node.take_event_receiver().expect("the host's events");
    node.start().await.expect("start the host");
    let node = Arc::new(node);
    let (forward, forwarded) = tokio::sync::mpsc::unbounded_channel();
    let tap = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if forward.send(event).is_err() {
                return;
            }
        }
    });
    let (end, ended) = tokio::sync::oneshot::channel();
    let lent = Lent {
        node: Arc::downgrade(&node),
        events: forwarded,
        ended,
    };
    (node, lent, tap, end)
}

async fn daemon() -> (ReticulumNode, tempfile::TempDir, String, SocketAddr) {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let instance = format!(
        "loquax-rooms-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .add_tcp_server("127.0.0.1:0".parse().expect("an address"))
        .storage_path(storage.path().to_path_buf())
        .build_sync()
        .expect("build the daemon");
    daemon.start().await.expect("start the daemon");
    let tcp = *daemon.tcp_listen_addrs().first().expect("a TCP listener");
    tokio::time::sleep(Duration::from_millis(500)).await;
    (daemon, storage, instance, tcp)
}

/// The next word from the link, within `secs`.
async fn next(told: &mut tokio::sync::mpsc::UnboundedReceiver<Told>, secs: u64) -> Told {
    tokio::time::timeout(Duration::from_secs(secs), told.recv())
        .await
        .expect("an answer in time")
        .expect("the link task runs")
}

#[tokio::test]
async fn loquax_links_to_a_hub_joins_says_a_line_and_knows_its_echo() {
    let (mut daemon, _daemon_storage, instance, tcp) = daemon().await;
    let hub = Hub::start(tcp).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let host_storage = tempfile::tempdir().expect("host storage");
    let (node, lent, tap, _end) = host(&instance, &host_storage).await;
    let identity = leviculum_std::generate_identity();
    let me = *identity.hash();
    let (lends, lends_rx) = tokio::sync::mpsc::unbounded_channel();
    lends.send(lent).expect("lend");
    let mut link = rooms_link::start(
        &tokio::runtime::Handle::current(),
        rooms_link::Options {
            lends: lends_rx,
            identity,
            nick: Some("lew".into()),
        },
        Arc::new(Notify::new()),
    );

    // Before a hub is open, a line is refused (the control for "sent").
    link.asks
        .send(Ask::Say {
            room: "#lab".into(),
            kind: LineKind::Msg,
            text: "early".into(),
        })
        .expect("ask");
    assert_eq!(
        next(&mut link.told, 5).await,
        Told::Refused(Refusal::NotReady)
    );

    link.asks.send(Ask::Open { hub: hub.dest }).expect("ask");
    assert_eq!(next(&mut link.told, 5).await, Told::Linking);
    assert_eq!(
        next(&mut link.told, 60).await,
        Told::Welcomed {
            name: Some("periculum".into()),
            body_limit: 350
        }
    );
    // Its rooms without a name typed (1652): `/list` after the WELCOME,
    // the answer's names as the hub spells them, nothing else told.
    assert_eq!(
        next(&mut link.told, 10).await,
        Told::Listed {
            rooms: vec!["general chat".into(), "lab".into()]
        }
    );

    // The hub's PING right after WELCOME is answered on the link and
    // tells the Rooms nothing: wait for the hub to have the PONG, so the
    // order the hub saw is the order asked.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !hub
        .seen
        .lock()
        .expect("seen")
        .envelopes
        .iter()
        .any(|e| e.kind == Kind::Pong)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no PONG reached the hub"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    link.asks
        .send(Ask::Join {
            room: "#Lab".into(),
            key: None,
        })
        .expect("ask");
    assert_eq!(
        next(&mut link.told, 10).await,
        Told::Joined {
            room: "#lab".into(),
            nick: None
        }
    );

    // A line over 350 bytes is refused before it is sent.
    link.asks
        .send(Ask::Say {
            room: "#lab".into(),
            kind: LineKind::Msg,
            text: "y".repeat(351),
        })
        .expect("ask");
    assert_eq!(
        next(&mut link.told, 5).await,
        Told::Refused(Refusal::TooLong {
            bytes: 351,
            limit: 350
        })
    );

    link.asks
        .send(Ask::Say {
            room: "#lab".into(),
            kind: LineKind::Msg,
            text: "rrc line, thirty-one bytes long".into(),
        })
        .expect("ask");
    let Told::Line {
        room,
        text,
        nick,
        ours,
        ..
    } = next(&mut link.told, 10).await
    else {
        panic!("the echo");
    };
    assert_eq!(
        (room.as_str(), text.as_str(), nick.as_deref()),
        ("#lab", "rrc line, thirty-one bytes long", Some("lew"))
    );
    assert!(ours, "the hub's echo of our line is ours");

    link.asks
        .send(Ask::Part {
            room: "#lab".into(),
        })
        .expect("ask");
    assert_eq!(
        next(&mut link.told, 10).await,
        Told::Parted {
            room: "#lab".into(),
            nick: None
        }
    );

    {
        let seen = hub.seen.lock().expect("seen");
        assert_eq!(
            seen.identified,
            [me],
            "the link was identified with the reader's identity"
        );
        let kinds: Vec<Kind> = seen.envelopes.iter().map(|e| e.kind).collect();
        // One HELLO (welcomed at once), `/list` (1652), the PONG, JOIN,
        // the one line that went, PART: the refused line never reached
        // the hub.
        assert_eq!(
            kinds,
            [
                Kind::Hello,
                Kind::Msg,
                Kind::Pong,
                Kind::Join,
                Kind::Msg,
                Kind::Part
            ]
        );
        assert_eq!(seen.envelopes[1].room, None, "a command names no room");
        assert_eq!(seen.envelopes[1].text(), Some("/list"));
        let ping = Envelope::decode(&vector("ping_down")).expect("ping");
        assert_eq!(
            seen.envelopes[2].body, ping.body,
            "the PONG hands the PING's body back"
        );
        assert!(seen
            .envelopes
            .iter()
            .all(|e| e.src.as_deref() == Some(&me[..])));
    }

    link.asks.send(Ask::Close).expect("ask");
    assert_eq!(next(&mut link.told, 5).await, Told::Down(Down::Left));

    tap.abort();
    hub.task.abort();
    drop(node);
    daemon.stop().await.expect("stop the daemon");
}

/// The control for the link: a hub nobody announced has no path, and the
/// link gives up with that, not with a welcome.
#[tokio::test]
async fn a_hub_without_a_path_is_given_up() {
    let (mut daemon, _daemon_storage, instance, _tcp) = daemon().await;
    let host_storage = tempfile::tempdir().expect("host storage");
    let (node, lent, tap, _end) = host(&instance, &host_storage).await;
    let (lends, lends_rx) = tokio::sync::mpsc::unbounded_channel();
    lends.send(lent).expect("lend");
    let mut link = rooms_link::start(
        &tokio::runtime::Handle::current(),
        rooms_link::Options {
            lends: lends_rx,
            identity: leviculum_std::generate_identity(),
            nick: None,
        },
        Arc::new(Notify::new()),
    );
    // No such destination: no path.
    link.asks.send(Ask::Open { hub: [0x42; 16] }).expect("ask");
    assert_eq!(next(&mut link.told, 5).await, Told::Linking);
    assert_eq!(next(&mut link.told, 45).await, Told::Down(Down::NoPath));
    tap.abort();
    drop(node);
    daemon.stop().await.expect("stop the daemon");
}

/// 1643, item 6: a mailbox that heard a hub asks it for its rooms (a
/// question of its own, the throwaway identity the sheet's question uses),
/// keeps them with the hub's archive entry, and a device that pulled the
/// entry lists them on the hub's sheet the moment it opens, before it
/// asked anything over a link of its own. Control: the entry the mailbox
/// kept before the answer carried no rooms, and a device that pulled only
/// that one opens the sheet empty.
#[cfg(feature = "history")]
#[tokio::test]
async fn a_mailbox_asks_a_heard_hub_for_its_rooms_and_the_sheet_lists_them_before_any_link() {
    use loquax::home::hub_rooms::{Answered, Lister};
    use loquax::ui::explore::ExploreEvent;
    let (_daemon, _daemon_storage, instance, tcp) = daemon().await;
    let hub = Hub::start(tcp).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mailbox_dir = tempfile::tempdir().expect("the mailbox's store");
    let mut store = loquax::store::Store::open_in(mailbox_dir.path(), [0x11; 16]).expect("store");
    let heard = loquax::ui::hubs::HeardHub {
        hash: hub.dest,
        name: Some("periculum".into()),
        hops: Some(1),
        first: 1_800_000_000,
        last: 1_800_000_000,
        answered: None,
    };
    store.archive_hub(&heard, u64::MAX).expect("heard");
    let before = hub_entry(&store);
    assert_eq!(before.1, None, "no rooms before the answer");

    let host_storage = tempfile::tempdir().expect("host storage");
    let (_node, lent, _tap, _end) = host(&instance, &host_storage).await;
    let (lends, lends_rx) = tokio::sync::mpsc::unbounded_channel();
    lends.send(lent).expect("lend");
    let mut lister = Lister::over(lends_rx);
    assert!(lister.heard(hub.dest, 1_800_000_000));
    assert!(!lister.heard(hub.dest, 1_800_000_060), "once an hour");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let answered = loop {
        let answers = lister.take(&mut store, u64::MAX);
        if let Some(answer) = answers.into_iter().next() {
            break answer;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hub never answered"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let Answered::Rooms {
        hub: asked,
        rooms,
        seq,
    } = answered
    else {
        panic!("the hub answered: {answered:?}");
    };
    assert_eq!(asked, hub.dest);
    assert!(seq.is_some(), "kept with the hub's entry");
    for room in ["general chat", "lab"] {
        assert!(rooms.iter().any(|r| r == room), "{room} in {rooms:?}");
    }
    assert!(
        hub.seen
            .lock()
            .expect("seen")
            .envelopes
            .iter()
            .any(|e| e.text() == Some("/list")),
        "the question was /list"
    );
    let after = hub_entry(&store);
    assert_eq!(after.1.as_deref(), Some(rooms.as_slice()));

    // The device: the pulled hub and its rooms, as the pull hands them on.
    let sheet = |entry: &(loquax::ui::hubs::HeardHub, Option<Vec<String>>)| {
        let mut model = loquax::ui::UiModel::<leviculum_ui::input::Line>::new(&[]);
        update_engine(
            &mut model,
            loquax::outbox::OutboxEvent::HubAnnounced {
                hub: entry.0.clone(),
            },
        );
        if let Some(rooms) = &entry.1 {
            update_engine(
                &mut model,
                loquax::outbox::OutboxEvent::HubRooms {
                    hub: entry.0.hash,
                    rooms: rooms.clone(),
                },
            );
        }
        loquax::ui::update(
            &mut model,
            loquax::ui::UiEvent::Explore(ExploreEvent::Hub(entry.0.hash)),
        );
        loquax::ui::explore::node(&model)
            .sheet
            .expect("the sheet")
            .rooms
    };
    assert_eq!(sheet(&after), rooms, "listed before any link");
    assert!(sheet(&before).is_empty(), "the control: nothing to list");
}

#[cfg(feature = "history")]
fn update_engine(
    model: &mut loquax::ui::UiModel<leviculum_ui::input::Line>,
    event: loquax::outbox::OutboxEvent,
) {
    loquax::ui::update(model, loquax::ui::UiEvent::Engine(event));
}

/// The newest hub entry the mailbox's archive keeps, with its rooms.
#[cfg(feature = "history")]
fn hub_entry(store: &loquax::store::Store) -> (loquax::ui::hubs::HeardHub, Option<Vec<String>>) {
    let page = store.archive_since(0, usize::MAX).expect("the archive");
    let entry = page
        .entries
        .iter()
        .rfind(|e| e.kind == loquax::home::KIND_HUB)
        .expect("a hub entry");
    loquax::home::wire::read_hub_entry_rooms(&entry.packed).expect("a hub entry")
}
