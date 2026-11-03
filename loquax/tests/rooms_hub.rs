//! The Rooms' hub link end to end (1379, #232): Loquax's link task over a
//! lent node, through a shared-instance daemon, to a hub on `rrc.hub`.
//!
//! The hub is a fake written from the RRC specification, with the
//! envelopes periculum 1378 recorded from the reference hub (rrcd 0.3.2)
//! as its answers: WELCOME and its PING are the recorded bytes, JOINED,
//! PARTED and the echo of a line are what the spec and the recording say
//! rrcd sends. The pinned rrcd itself lives in periculum's netns cells and
//! needs Python, which this harness does not start.
//!
//! ```text
//! rooms_link ── lent node ── Unix socket ── daemon ── TCP ── fake hub
//! ```

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_rrc::{Envelope, Kind, LineKind, Refusal, Value};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{
    Destination, DestinationType, Direction, NodeEvent, ProofStrategy, ReticulumNode,
};
use loquax::engine::Lent;
use loquax::rooms_link::{self, Ask, Down, Told};
use tokio::sync::Notify;

fn vector(name: &str) -> Vec<u8> {
    let file: serde_json::Value = serde_json::from_str(include_str!(
        "../../leviculum-rrc/tests/vectors/envelopes.json"
    ))
    .expect("the vectors");
    let v = file["vectors"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no vector {name}"));
    hex::decode(v["cbor"].as_str().expect("cbor")).expect("hex")
}

/// What the fake hub saw.
#[derive(Default)]
struct Seen {
    /// The identity each link identified with.
    identified: Vec<[u8; 16]>,
    /// Every envelope a client sent, decoded.
    envelopes: Vec<Envelope>,
}

/// The fake hub: one destination, the spec's answers.
struct Hub {
    dest: [u8; 16],
    seen: Arc<Mutex<Seen>>,
    task: tokio::task::JoinHandle<()>,
    _storage: tempfile::TempDir,
}

impl Hub {
    async fn start(daemon_tcp: SocketAddr) -> Hub {
        let storage = tempfile::tempdir().expect("hub storage");
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(daemon_tcp)
            .storage_path(storage.path().to_path_buf())
            .build_sync()
            .expect("build the hub");
        let events = node.take_event_receiver().expect("the hub's events");
        node.start().await.expect("start the hub");
        let mut destination = Destination::new(
            Some(leviculum_std::generate_identity()),
            Direction::In,
            DestinationType::Single,
            leviculum_rrc::HUB_APP,
            &[leviculum_rrc::HUB_ASPECT],
        )
        .expect("the hub destination");
        destination.set_accepts_links(true);
        // rrcd sets no proof strategy: PROVE_NONE (the study, 2026-11-03).
        destination.set_proof_strategy(ProofStrategy::None);
        let dest = *destination.hash();
        node.register_destination(destination);
        // The announce's app data as rrcd's: {"proto": "rrc", "v": 1, "hub": name}.
        let app_data = Value::Map(vec![
            (Value::Text("proto".into()), Value::Text("rrc".into())),
            (Value::Text("v".into()), Value::Uint(1)),
            (Value::Text("hub".into()), Value::Text("fake".into())),
        ])
        .encode();
        node.announce_destination(&dest, Some(&app_data))
            .await
            .expect("announce the hub");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let task = tokio::spawn(serve(node, events, Arc::clone(&seen)));
        Hub {
            dest: *dest.as_bytes(),
            seen,
            task,
            _storage: storage,
        }
    }
}

/// The hub's side, as the spec and the recording say: WELCOME to a
/// HELLO, then a PING; JOINED to a JOIN (without a nick to the joiner);
/// a line back to its sender unchanged but for key 4, the link's
/// identity; PARTED to a PART.
async fn serve(
    node: ReticulumNode,
    mut events: leviculum_std::EventReceiver,
    seen: Arc<Mutex<Seen>>,
) {
    let mut who = std::collections::HashMap::new();
    while let Some(event) = events.recv().await {
        match event {
            NodeEvent::LinkIdentified {
                link_id,
                identity_hash,
            } => {
                who.insert(link_id, identity_hash);
                seen.lock().expect("seen").identified.push(identity_hash);
            }
            NodeEvent::LinkDataReceived { link_id, data } => {
                let Ok(mut envelope) = Envelope::decode(&data) else {
                    continue;
                };
                seen.lock().expect("seen").envelopes.push(envelope.clone());
                let mut answers = Vec::new();
                match envelope.kind {
                    Kind::Hello => {
                        answers.push(vector("welcome_down"));
                        answers.push(vector("ping_down"));
                    }
                    Kind::Join | Kind::Part => {
                        let name = if envelope.kind == Kind::Join {
                            "joined_down"
                        } else {
                            "parted_down"
                        };
                        let mut answer = Envelope::decode(&vector(name)).expect("a vector");
                        answer.room.clone_from(&envelope.room);
                        answers.push(answer.encode());
                    }
                    Kind::Msg | Kind::Notice | Kind::Action => {
                        envelope.src = who.get(&link_id).map(|hash: &[u8; 16]| hash.to_vec());
                        answers.push(envelope.encode());
                    }
                    _ => {}
                }
                for answer in answers {
                    let _ = node.send_link_packet(&link_id, &answer).await;
                }
            }
            _ => {}
        }
    }
}

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
        // One HELLO (welcomed at once), the PONG, JOIN, the one line that
        // went, PART: the refused line never reached the hub.
        assert_eq!(
            kinds,
            [Kind::Hello, Kind::Pong, Kind::Join, Kind::Msg, Kind::Part]
        );
        let ping = Envelope::decode(&vector("ping_down")).expect("ping");
        assert_eq!(
            seen.envelopes[1].body, ping.body,
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
