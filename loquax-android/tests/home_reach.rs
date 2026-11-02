//! Reaching the mailbox from anywhere (1196,
//! `docs/src/concepts/loquax-reachability.md`): the phone's leaf on a
//! relay's access point port, the mailbox's own node (`DaemonUpstream::Mesh`,
//! as on a phone or a server without lnsd) on a relay's ordinary port, both
//! dialling through `loquax::home::rendezvous`. Every cell runs the same four
//! steps, join, watch, send and pull, and they mirror the periculum cells
//! `home_reach_same_node`, `home_reach_two_nodes_tcp` and the failover.
//!
//! Each relay is a Rust transport node in this process with a TCP server in
//! `full` mode (the peer port) and one in `access_point` mode (the phones').

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use leviculum_core::DestinationHash;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use loquax::engine::{attach_leaf, Attached, LeafConfig, Role};
use loquax::home::client;
use loquax::home::rendezvous::Timing;
use loquax::home::watch::{self, WatchEvent};

#[path = "../../loquax/tests/support/python_peer.rs"]
mod python_peer;

/// loquax's own lines on stderr: the rotation's `HOME_RENDEZVOUS` and
/// `HOME_PATH_REQUEST` beside the client's, which a red cell shows.
fn logs() {
    leviculum_std::event_log::install_global_subscriber("warn,loquax=info");
}

/// The failover's waits, short enough for a test and in the shipped ratio.
fn fast() -> Timing {
    Timing {
        dial_grace_ms: 2_000,
        backoff_first_ms: 200,
        backoff_max_ms: 1_000,
        return_first_ms: 3_000,
        return_max_ms: 6_000,
        up_look_ms: 300,
        dial_look_ms: 100,
        network_settle_ms: 300,
        network_settle_max_ms: 1_500,
    }
}

/// A transport node with a peer port and a phones' port in access point
/// mode, and TCP clients to `uplinks`.
struct Relay {
    node: ReticulumNode,
    peer: SocketAddr,
    ap: SocketAddr,
    _dir: tempfile::TempDir,
}

impl Relay {
    async fn start(uplinks: &[SocketAddr]) -> Self {
        let peer = TcpListener::bind("127.0.0.1:0").expect("a peer port");
        let ap = TcpListener::bind("127.0.0.1:0").expect("an access point port");
        Self::on(peer, ap, uplinks).await
    }

    async fn on(peer: TcpListener, ap: TcpListener, uplinks: &[SocketAddr]) -> Self {
        let (peer_addr, ap_addr) = (
            peer.local_addr().expect("peer"),
            ap.local_addr().expect("ap"),
        );
        let dir = tempfile::tempdir().expect("relay dir");
        let mut builder = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .add_tcp_server_listener(peer)
            .add_tcp_server_listener(ap)
            .interface_mode("access_point");
        for uplink in uplinks {
            builder = builder.add_tcp_client(*uplink);
        }
        let mut node = builder
            .storage_path(dir.path().to_path_buf())
            .build()
            .await
            .expect("the relay");
        node.start().await.expect("start the relay");
        Self {
            node,
            peer: peer_addr,
            ap: ap_addr,
            _dir: dir,
        }
    }

    async fn stop(mut self) -> (SocketAddr, SocketAddr) {
        self.node.stop().await.expect("stop the relay");
        (self.peer, self.ap)
    }
}

/// The mailbox with a node of its own, dialling `servers` as a peer.
struct Mailbox {
    dir: tempfile::TempDir,
    public_key: [u8; 64],
    token: [u8; 16],
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

impl Mailbox {
    async fn start(servers: Vec<String>) -> Self {
        let dir = tempfile::tempdir().expect("mailbox dir");
        let identity =
            loquax::identity::load_or_create(&dir.path().join("identity")).expect("identity");
        let address = loquax::engine::delivery_address(&identity).expect("address");
        let token = [0x42; 16];
        loquax::store::Store::open_in(dir.path(), address)
            .expect("the store")
            .issue_token(&token, "phone", leviculum_std::local_time::unix_now_secs())
            .expect("a token");
        let storage = dir.path().join("storage");
        std::fs::create_dir_all(&storage).expect("storage");
        let public_key = identity.public_key_bytes();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(loquax::home::daemon::run(
            loquax::home::daemon::DaemonOptions {
                home: dir.path().to_path_buf(),
                upstream: loquax::home::daemon::DaemonUpstream::Mesh {
                    servers,
                    screens: TcpListener::bind("127.0.0.1:0").expect("screens"),
                    tcp: Default::default(),
                    timing: fast(),
                },
                storage_dir: storage,
                identity,
                display_name: b"mailbox".to_vec(),
                archive_cap_bytes: 1 << 20,
                token_ttl_secs: 3600,
                policy: loquax::home::policy(&loquax::config::Config::default()),
                preferred_pn: None,
                pn_source: "none",
                sync_bound_ms: None,
                fetch_budget: Duration::from_secs(10),
                wake: true,
                stop: Some(stopped),
                network: Default::default(),
            },
        ));
        Self {
            dir,
            public_key,
            token,
            stop,
            task,
        }
    }

    fn home(&self) -> DestinationHash {
        DestinationHash::new(loquax::home::destination_hash(&self.public_key).expect("home"))
    }

    /// The `lxmf.delivery` address a sender on the mesh writes to.
    fn delivery(&self) -> DestinationHash {
        DestinationHash::new(loquax::home::lxmf_address(&self.public_key).expect("address"))
    }

    async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
        drop(self.dir);
    }
}

/// The phone's leaf, dialling `servers`; `ask` says whether it asks for
/// the home destination's path on every up (rule 2), off only for the
/// positive control.
struct Phone {
    dir: tempfile::TempDir,
    device: leviculum_core::Identity,
    attached: Attached,
}

impl Phone {
    async fn start(servers: Vec<String>, mailbox: &Mailbox, ask: bool) -> Self {
        let dir = tempfile::tempdir().expect("phone dir");
        let storage = dir.path().join("storage");
        std::fs::create_dir_all(&storage).expect("storage");
        let device = leviculum_core::Identity::generate(&mut rand_core::OsRng);
        let address = loquax::home::lxmf_address(&mailbox.public_key).expect("address");
        let attached = attach_leaf(
            LeafConfig {
                servers,
                home_destination: ask.then(|| *mailbox.home().as_bytes()),
                timing: fast(),
                storage_dir: storage,
                identity: device.clone(),
                display_name: Vec::new(),
                notices: loquax::effects::Terminal::new(std::env::temp_dir()),
                tcp: Default::default(),
                network: Default::default(),
            },
            Role::Client { address },
        )
        .await
        .expect("the leaf");
        Self {
            dir,
            device,
            attached,
        }
    }
}

/// The entry whose connection is up, as the rotation says, if any.
fn active(attached: &Attached) -> Option<String> {
    attached
        .rendezvous()
        .filter(|state| state.up)
        .map(|state| state.entry)
}

async fn wait_until(what: &str, secs: u64, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: not within {secs} s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn next_watch(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<WatchEvent>,
    secs: u64,
    want: impl Fn(&WatchEvent) -> bool,
) -> WatchEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| panic!("no wanted watch event within {secs} s"))
            .expect("the watch runs");
        if want(&event) {
            return event;
        }
    }
}

/// The four steps of every cell over `phone`: join, a watching link, a
/// send that wakes it, and a pull that brings the sent entry. `budget`
/// bounds each request, as the 1123 bounds do for the medium.
async fn four_steps(phone: &Phone, mailbox: &Mailbox, budget: Duration) {
    let node = phone.attached.node();
    client::join(
        &node,
        &mailbox.public_key,
        &phone.device,
        &mailbox.token,
        "phone",
        budget,
    )
    .await
    .unwrap_or_else(|error| panic!("join: {error}"));
    client::save_client(phone.dir.path(), &mailbox.public_key).expect("client file");
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let watching = tokio::spawn(watch::run(
        watch::Watch {
            node: node.clone(),
            public_key: mailbox.public_key,
            device: phone.device.clone(),
            home: phone.dir.path().to_path_buf(),
        },
        tx,
    ));
    next_watch(&mut events, budget.as_secs(), |e| {
        *e == WatchEvent::Watching(true)
    })
    .await;
    let WatchEvent::Wake(before) =
        next_watch(&mut events, 5, |e| matches!(e, WatchEvent::Wake(_))).await
    else {
        unreachable!()
    };
    let id = client::send(
        &node,
        &mailbox.public_key,
        &phone.device,
        &loquax::home::wire::Submission {
            client_id: client::client_id(),
            destination: [0x5a; 16],
            title: Vec::new(),
            content: b"durch den Treffpunkt".to_vec(),
            fields: Vec::new(),
        },
        budget,
    )
    .await
    .unwrap_or_else(|error| panic!("send: {error}"));
    next_watch(
        &mut events,
        budget.as_secs(),
        |e| matches!(e, WatchEvent::Wake(n) if *n > before),
    )
    .await;
    let pulled = client::pull(&node, &mailbox.public_key, &phone.device, 0, budget)
        .await
        .unwrap_or_else(|error| panic!("pull: {error}"));
    assert!(
        pulled.entries.iter().any(|entry| {
            entry.kind == loquax::home::KIND_SENT
                && leviculum_lxmf::Message::unpack(
                    &entry.packed,
                    None,
                    None,
                    leviculum_lxmf::DeliveryMethod::Direct,
                )
                .is_ok_and(|message| message.message_id == id)
        }),
        "the sent message is in the pull: {pulled:?}"
    );
    drop(events);
    watching.abort();
}

fn entry(addr: SocketAddr) -> String {
    format!("{addr}")
}

/// `home_reach_same_node`: phone (access point port) and mailbox (peer port)
/// on one rendezvous. The phone asks for the path the moment its entry is
/// up, before any request of its own: the path is there without a client
/// call.
#[tokio::test(flavor = "multi_thread")]
async fn home_reach_same_node() {
    logs();
    let relay = Relay::start(&[]).await;
    let mailbox = Mailbox::start(vec![entry(relay.peer)]).await;
    let phone = Phone::start(vec![entry(relay.ap)], &mailbox, true).await;
    let node = phone.attached.node();
    let home = mailbox.home();
    wait_until("the phone's rendezvous is up", 10, || {
        active(&phone.attached).is_some()
    })
    .await;
    wait_until("the path the rotation asked for", 10, || {
        node.has_path(&home)
    })
    .await;
    four_steps(&phone, &mailbox, Duration::from_secs(30)).await;
    drop(node);
    phone.attached.stop().await;
    mailbox.stop().await;
    relay.stop().await;
}

/// `home_reach_two_nodes_tcp`: the phone on node A's access point port, the
/// mailbox on node B's peer port, A and B linked by TCP only. Positive
/// control first: the phone hears no announce of `loquax.home` through the
/// access point port though A holds its path from the mailbox's announce,
/// so a client that waited for an announce would never link; an explicit
/// path request gets it at once. Then the phone that asks on every up runs
/// the four steps.
#[tokio::test(flavor = "multi_thread")]
async fn home_reach_two_nodes_tcp() {
    logs();
    let b = Relay::start(&[]).await;
    let a = Relay::start(&[b.peer]).await;
    let mailbox = Mailbox::start(vec![entry(b.peer)]).await;
    let home = mailbox.home();
    // The mailbox's announce of its home destination (1196) reached A.
    wait_until("node A holds the mailbox's path", 15, || {
        a.node.has_path(&home)
    })
    .await;
    assert_eq!(a.node.hops_to(&home), Some(2), "A -> B -> mailbox");

    let silent = Phone::start(vec![entry(a.ap)], &mailbox, false).await;
    let node = silent.attached.node();
    wait_until("the silent phone's rendezvous is up", 10, || {
        active(&silent.attached).is_some()
    })
    .await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !node.has_path(&home),
        "the access point port forwarded the mailbox's announce to the phone"
    );
    node.request_path(&home).await.expect("a path request");
    wait_until("the answer to the explicit request", 10, || {
        node.has_path(&home)
    })
    .await;
    assert_eq!(node.hops_to(&home), Some(3), "phone -> A -> B -> mailbox");
    drop(node);
    silent.attached.stop().await;

    let phone = Phone::start(vec![entry(a.ap)], &mailbox, true).await;
    let node = phone.attached.node();
    wait_until("the path the rotation asked for", 10, || {
        node.has_path(&home)
    })
    .await;
    four_steps(&phone, &mailbox, Duration::from_secs(30)).await;
    drop(node);
    phone.attached.stop().await;
    mailbox.stop().await;
    a.stop().await;
    b.stop().await;
}

/// The catch under way A (`loquax-reachability.md`): a separate relay as
/// node B and the mailbox one hop behind it, so the phone's request crosses
/// A, B and C on plain ports beyond A. With the home destination announced
/// every relay holds the path, and the four steps run.
#[tokio::test(flavor = "multi_thread")]
async fn home_reach_beyond_the_first_relay() {
    logs();
    let c = Relay::start(&[]).await;
    let b = Relay::start(&[c.peer]).await;
    let a = Relay::start(&[b.peer]).await;
    let mailbox = Mailbox::start(vec![entry(c.peer)]).await;
    let home = mailbox.home();
    wait_until("node B holds the mailbox's path", 15, || {
        b.node.has_path(&home)
    })
    .await;
    let phone = Phone::start(vec![entry(a.ap)], &mailbox, true).await;
    let node = phone.attached.node();
    wait_until("the path the rotation asked for", 15, || {
        node.has_path(&home)
    })
    .await;
    assert_eq!(
        node.hops_to(&home),
        Some(4),
        "phone -> A -> B -> C -> mailbox"
    );
    four_steps(&phone, &mailbox, Duration::from_secs(30)).await;
    drop(node);
    phone.attached.stop().await;
    mailbox.stop().await;
    a.stop().await;
    b.stop().await;
    c.stop().await;
}

/// A mailbox whose entry answers only after its start (1274): the delivery
/// announce of the registration went out on no interface, and a sender
/// beside the entry learns the mailbox's address only if the mailbox
/// announces it again when the entry comes up. The facade's Python sender
/// lost that race under the land gate's load and never learned the address.
#[tokio::test(flavor = "multi_thread")]
async fn home_reach_a_late_entry_hears_the_delivery_announce() {
    logs();
    let peer = TcpListener::bind("127.0.0.1:0").expect("a peer port");
    let peer_addr = peer.local_addr().expect("peer");
    drop(peer);
    let mailbox = Mailbox::start(vec![entry(peer_addr)]).await;
    // Long past the registration's announce, and inside the first grace.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let relay = Relay::on(
        TcpListener::bind(peer_addr).expect("the peer port, now answering"),
        TcpListener::bind("127.0.0.1:0").expect("an access point port"),
        &[],
    )
    .await;
    let (delivery, home) = (mailbox.delivery(), mailbox.home());
    wait_until("the relay holds the mailbox's delivery path", 10, || {
        relay.node.has_path(&delivery)
    })
    .await;
    wait_until("the relay holds the mailbox's home path", 10, || {
        relay.node.has_path(&home)
    })
    .await;
    mailbox.stop().await;
    relay.stop().await;
}

/// A stock Python RNS as the mailbox's entry (1274): the announces the
/// mailbox sends when its entry comes up must not trip Python's ingress
/// control. A new interface that brings three announces of unknown
/// destinations in a burst has the third held
/// (`reference/Reticulum/RNS/Interfaces/Interface.py:145`,
/// `RNS/Transport.py:1705`), released after a 15 s penalty and held again
/// until six announces were counted, so a sender beside the entry learns
/// a held delivery address only after half a minute or more. Which
/// announce comes third depends on how late the engine's first tick is, so
/// the cell counts: one announce per destination, two in all, never three.
#[tokio::test(flavor = "multi_thread")]
async fn home_reach_a_python_entry_holds_none_of_the_mailboxs_announces() {
    logs();
    let peer = python_peer::PythonPeer::start();
    let mailbox = Mailbox::start(vec![format!("127.0.0.1:{}", peer.rns_port)]).await;
    let (delivery, home) = (mailbox.delivery(), mailbox.home());
    let knows = |hash: &DestinationHash| {
        peer.rpc(
            "has_path",
            serde_json::json!({ "hash": loquax::address::to_hex(hash.as_bytes()) }),
        ) == serde_json::json!(true)
    };
    wait_until("Python holds the mailbox's delivery path", 10, || {
        knows(&delivery)
    })
    .await;
    wait_until("Python holds the mailbox's home path", 10, || knows(&home)).await;
    // Whatever the connect brought has arrived by now.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let interfaces = peer.rpc("get_interfaces", serde_json::json!({}));
    let held: Vec<&serde_json::Value> = interfaces
        .as_array()
        .expect("a list")
        .iter()
        .filter(|i| {
            i.get("held_announces").and_then(|v| v.as_u64()) != Some(0)
                || i.get("burst_active") == Some(&serde_json::json!(true))
        })
        .collect();
    assert!(held.is_empty(), "Python held announces: {held:?}");
    let most = interfaces
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|i| i.get("announces_in").and_then(|v| v.as_u64()))
        .max();
    assert_eq!(
        most,
        Some(2),
        "the delivery and the home announce, once each: {interfaces}"
    );
    mailbox.stop().await;
}

/// Failover: rendezvous one down, both ends come up on rendezvous two within
/// the grace and the backoff and the steps run there; rendezvous one back,
/// both return to it on the return try and the steps run there again.
#[tokio::test(flavor = "multi_thread")]
async fn home_reach_fails_over_and_returns() {
    logs();
    let one = Relay::start(&[]).await;
    let two = Relay::start(&[]).await;
    let mailbox = Mailbox::start(vec![entry(one.peer), entry(two.peer)]).await;
    let phone = Phone::start(vec![entry(one.ap), entry(two.ap)], &mailbox, true).await;
    let node = phone.attached.node();
    let home = mailbox.home();
    let (one_peer, one_ap) = (one.peer, one.ap);
    wait_until("the phone on rendezvous one", 10, || {
        active(&phone.attached) == Some(entry(one_ap))
    })
    .await;
    wait_until("the path through rendezvous one", 10, || {
        node.has_path(&home)
    })
    .await;
    four_steps(&phone, &mailbox, Duration::from_secs(30)).await;

    one.stop().await;
    // The grace, the backoff and a dial: well inside ten seconds.
    wait_until("the phone on rendezvous two", 10, || {
        active(&phone.attached) == Some(entry(two.ap))
    })
    .await;
    wait_until("the path through rendezvous two", 10, || {
        node.has_path(&home) && two.node.has_path(&home)
    })
    .await;
    // A sender beside rendezvous two writes to the delivery address, which
    // the mailbox announces on its new entry too (1274).
    let delivery = mailbox.delivery();
    wait_until("rendezvous two holds the delivery path", 10, || {
        two.node.has_path(&delivery)
    })
    .await;
    // The cursor of the first pass is kept; the steps start over.
    std::fs::remove_file(phone.dir.path().join(client::CURSOR_FILE)).ok();
    four_steps_again(&phone, &mailbox).await;

    let one = Relay::on(
        TcpListener::bind(one_peer).expect("rendezvous one's peer port again"),
        TcpListener::bind(one_ap).expect("rendezvous one's access point port again"),
        &[],
    )
    .await;
    // The return try after `return_first_ms`, then the dial.
    wait_until("the phone back on rendezvous one", 15, || {
        active(&phone.attached) == Some(entry(one_ap))
    })
    .await;
    wait_until("the mailbox back on rendezvous one", 15, || {
        one.node.has_path(&home)
    })
    .await;
    wait_until("the path through rendezvous one again", 10, || {
        node.has_path(&home)
    })
    .await;
    four_steps_again(&phone, &mailbox).await;

    drop(node);
    phone.attached.stop().await;
    mailbox.stop().await;
    one.stop().await;
    two.stop().await;
}

/// The steps after the first join: a watching link, a send that wakes it, a
/// pull. The token is spent, so no second join.
async fn four_steps_again(phone: &Phone, mailbox: &Mailbox) {
    let node = phone.attached.node();
    let budget = Duration::from_secs(30);
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let watching = tokio::spawn(watch::run(
        watch::Watch {
            node: node.clone(),
            public_key: mailbox.public_key,
            device: phone.device.clone(),
            home: phone.dir.path().to_path_buf(),
        },
        tx,
    ));
    next_watch(&mut events, 30, |e| *e == WatchEvent::Watching(true)).await;
    let WatchEvent::Wake(before) =
        next_watch(&mut events, 5, |e| matches!(e, WatchEvent::Wake(_))).await
    else {
        unreachable!()
    };
    client::send(
        &node,
        &mailbox.public_key,
        &phone.device,
        &loquax::home::wire::Submission {
            client_id: client::client_id(),
            destination: [0x5b; 16],
            title: Vec::new(),
            content: b"wieder da".to_vec(),
            fields: Vec::new(),
        },
        budget,
    )
    .await
    .unwrap_or_else(|error| panic!("send: {error}"));
    next_watch(
        &mut events,
        30,
        |e| matches!(e, WatchEvent::Wake(n) if *n > before),
    )
    .await;
    let pulled = client::pull(&node, &mailbox.public_key, &phone.device, before, budget)
        .await
        .unwrap_or_else(|error| panic!("pull: {error}"));
    assert!(!pulled.entries.is_empty(), "{pulled:?}");
    drop(events);
    watching.abort();
}

// ---------------------------------------------------------------------------
// A network change (1298): the platform's word, `UiEffect::Network`, reaches
// the rotation through `loquax::home::rendezvous::Network`. The servers here
// are fakes, bare TCP listeners that only count who connects and when a
// connection ends, so the dial order and the bound are what the test sees.

/// What the fakes saw, in order: `A+` a connection to A, `A-` its end.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// A rendezvous that answers TCP and nothing else.
struct Fake {
    name: &'static str,
    addr: SocketAddr,
    seen: Seen,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Fake {
    async fn start(name: &'static str, seen: &Seen) -> Self {
        let mut fake = Self {
            name,
            addr: "127.0.0.1:0".parse().expect("an address"),
            seen: std::sync::Arc::clone(seen),
            task: None,
        };
        fake.up().await;
        fake
    }

    /// Listen again, on the port it had.
    async fn up(&mut self) {
        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .expect("the fake's port");
        self.addr = listener.local_addr().expect("the fake's address");
        let (name, seen) = (self.name, std::sync::Arc::clone(&self.seen));
        self.task = Some(tokio::spawn(async move {
            let mut held = tokio::task::JoinSet::new();
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                seen.lock().expect("seen").push(format!("{name}+"));
                let seen = std::sync::Arc::clone(&seen);
                // Dropped with the set when the fake goes down, which ends
                // the connection from this side.
                held.spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0u8; 512];
                    while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
                    seen.lock().expect("seen").push(format!("{name}-"));
                });
            }
        }));
    }

    /// Stop listening and end every connection.
    async fn down(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

fn seen(seen: &Seen) -> Vec<String> {
    seen.lock().expect("seen").clone()
}

/// How many connections `name` took so far.
fn dials(seen: &Seen, name: &str) -> usize {
    let plus = format!("{name}+");
    self::seen(seen).iter().filter(|s| **s == plus).count()
}

/// A leaf over `servers` that hears `network`.
async fn leaf(
    servers: Vec<String>,
    timing: Timing,
    network: loquax::home::rendezvous::Network,
) -> (tempfile::TempDir, Attached) {
    let dir = tempfile::tempdir().expect("phone dir");
    let storage = dir.path().join("storage");
    std::fs::create_dir_all(&storage).expect("storage");
    let attached = attach_leaf(
        LeafConfig {
            servers,
            home_destination: Some([0x6c; 16]),
            timing,
            storage_dir: storage,
            identity: leviculum_core::Identity::generate(&mut rand_core::OsRng),
            display_name: Vec::new(),
            notices: loquax::effects::Terminal::new(std::env::temp_dir()),
            tcp: Default::default(),
            network,
        },
        Role::Client {
            address: [0x6d; 16],
        },
    )
    .await
    .expect("the leaf");
    (dir, attached)
}

/// Nothing new in `seen` for `ms`: what a bound or a pause promises.
async fn quiet_for(seen_now: &Seen, ms: u64, what: &str) {
    let before = seen(seen_now);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    assert_eq!(seen(seen_now), before, "{what}");
}

/// The five changes of the order, time compressed, against two fakes A
/// and B: each one that asks for it drops the connection and dials the
/// first entry at once, a burst is one dial, no network dials nothing,
/// and the same network with the connection up keeps it. The return to
/// A after a failover is set far out, so a dial of A in the fifth change
/// can only be the evaluation's.
#[tokio::test(flavor = "multi_thread")]
async fn network_changes_redial_the_first_entry_once_each() {
    logs();
    let log: Seen = Seen::default();
    let mut a = Fake::start("A", &log).await;
    let b = Fake::start("B", &log).await;
    let timing = Timing {
        return_first_ms: 600_000,
        return_max_ms: 600_000,
        ..fast()
    };
    let network = loquax::home::rendezvous::Network::new();
    let (_dir, phone) = leaf(vec![entry(a.addr), entry(b.addr)], timing, network.clone()).await;
    let entry_a = entry(a.addr);
    let entry_b = entry(b.addr);
    let up_on = |want: &str| active(&phone).as_deref() == Some(want);
    wait_until("the first dial is up on A", 10, || up_on(&entry_a)).await;
    assert_eq!(seen(&log), ["A+"]);
    assert!(!network.reconnecting());

    // 1. WLAN off and on: no network drops A and dials nothing, the
    //    WLAN back dials A once.
    network.say(loquax::home::NetworkKind::None, true);
    wait_until("no network drops A", 5, || seen(&log).len() == 2).await;
    assert_eq!(seen(&log), ["A+", "A-"]);
    assert!(network.reconnecting(), "the line says Reconnecting");
    quiet_for(&log, 1_500, "no network dials nothing").await;
    network.say(loquax::home::NetworkKind::Wifi, true);
    wait_until("the WLAN back dials A", 5, || up_on(&entry_a)).await;
    assert_eq!(seen(&log), ["A+", "A-", "A+"]);
    wait_until("the line is gone", 5, || !network.reconnecting()).await;

    // 2. WLAN to mobile data: the platform says lost and available, a
    //    WLAN at the edge of its reach comes and goes once more, then
    //    mobile data and its capabilities, all within the settle; one
    //    drop and one dial of A for all of it. Each word on its own would
    //    have dropped and dialled A twice more.
    for (kind, changed) in [
        (loquax::home::NetworkKind::None, true),
        (loquax::home::NetworkKind::Wifi, true),
        (loquax::home::NetworkKind::None, true),
        (loquax::home::NetworkKind::Mobile, true),
        (loquax::home::NetworkKind::Mobile, false),
    ] {
        network.say(kind, changed);
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    wait_until("mobile data dials A", 5, || dials(&log, "A") == 3).await;
    wait_until("up on A", 5, || up_on(&entry_a)).await;
    quiet_for(&log, 1_500, "the burst was one dial").await;
    assert_eq!(seen(&log), ["A+", "A-", "A+", "A-", "A+"]);

    // 3. Flight mode for "two minutes" (two seconds here): nothing is
    //    dialled while it lasts, one dial when it ends.
    network.say(loquax::home::NetworkKind::None, true);
    wait_until("flight mode drops A", 5, || seen(&log).len() == 6).await;
    quiet_for(&log, 2_000, "flight mode dials nothing").await;
    network.say(loquax::home::NetworkKind::Mobile, true);
    wait_until("the end of flight mode dials A", 5, || up_on(&entry_a)).await;
    assert_eq!(dials(&log, "A"), 4);

    // 4. A new WLAN with another subnet: the connection of the old one is
    //    dropped and A dialled anew, though it looked up.
    network.say(loquax::home::NetworkKind::Wifi, true);
    wait_until("the new WLAN dials A", 5, || dials(&log, "A") == 5).await;
    wait_until("up on A", 5, || up_on(&entry_a)).await;
    assert_eq!(
        seen(&log),
        ["A+", "A-", "A+", "A-", "A+", "A-", "A+", "A-", "A+"]
    );

    // 5. Asleep, and the same network back: with the connection up it is
    //    kept (no dial, no drop), however often it is said.
    for _ in 0..3 {
        network.say(loquax::home::NetworkKind::Wifi, false);
    }
    quiet_for(&log, 1_500, "the same network with A up keeps A").await;
    assert!(up_on(&entry_a));
    assert!(!network.reconnecting());

    // The order: A gone, the rotation fails over to B. A answers again,
    // and a change dials the first entry, A, not the B that carries it.
    a.down().await;
    wait_until("the failover to B", 15, || up_on(&entry_b)).await;
    a.up().await;
    let before = seen(&log).len();
    network.say(loquax::home::NetworkKind::Wifi, true);
    wait_until("the change dials A first", 5, || up_on(&entry_a)).await;
    let after: Vec<String> = seen(&log)[before..].to_vec();
    assert_eq!(after.first().map(String::as_str), Some("B-"), "{after:?}");
    assert!(after.contains(&"A+".to_string()), "{after:?}");
    assert!(!after.contains(&"B+".to_string()), "{after:?}");

    phone.stop().await;
    a.down().await;
    let mut b = b;
    b.down().await;
}

/// The first wake after Doze: the same network, the connection down and a
/// long backoff running. The evaluation cuts the backoff short and dials
/// the first entry at once.
#[tokio::test(flavor = "multi_thread")]
async fn the_wake_after_doze_cuts_the_backoff_short() {
    logs();
    let log: Seen = Seen::default();
    let mut a = Fake::start("A", &log).await;
    let timing = Timing {
        backoff_first_ms: 60_000,
        backoff_max_ms: 60_000,
        ..fast()
    };
    let network = loquax::home::rendezvous::Network::new();
    let (_dir, phone) = leaf(vec![entry(a.addr)], timing, network.clone()).await;
    let entry_a = entry(a.addr);
    let up_on = |want: &str| active(&phone).as_deref() == Some(want);
    wait_until("up on A", 10, || up_on(&entry_a)).await;
    // Asleep: the connection died and the rotation waits out a minute.
    a.down().await;
    wait_until("A is down", 10, || active(&phone).is_none()).await;
    tokio::time::sleep(Duration::from_millis(
        fast().dial_grace_ms + fast().dial_look_ms * 3,
    ))
    .await;
    a.up().await;
    quiet_for(&log, 1_000, "the backoff holds the dial").await;
    let dialled = dials(&log, "A");
    network.say(loquax::home::NetworkKind::Wifi, false);
    wait_until("the wake dials A at once", 5, || up_on(&entry_a)).await;
    assert_eq!(dials(&log, "A"), dialled + 1, "{:?}", seen(&log));
    phone.stop().await;
    a.down().await;
}
