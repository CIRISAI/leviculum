//! The mailbox-reach harness (1196, 1354): relays with a peer and an
//! access point port, a mailbox on a node of its own
//! (`DaemonUpstream::Mesh`) and a phone's leaf, and the four steps every
//! cell runs over them. Shared by loquax-android's `home_reach` and
//! loquax's `mailbox_mesh`, the host's mailbox on its own node.

// Each test crate includes this module and uses its own part of it.
#![allow(dead_code)]

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use leviculum_core::DestinationHash;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use loquax::engine::{attach_leaf, Attached, LeafConfig, Role};
use loquax::home::client;
use loquax::home::rendezvous::Timing;
use loquax::home::watch::{self, WatchEvent};

/// loquax's own lines on stderr: the rotation's `HOME_RENDEZVOUS` and
/// `HOME_PATH_REQUEST` beside the client's, which a red cell shows.
pub fn logs() {
    leviculum_std::event_log::install_global_subscriber("warn,loquax=info");
}

/// The failover's waits, short enough for a test and in the shipped ratio.
pub fn fast() -> Timing {
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
pub struct Relay {
    pub node: ReticulumNode,
    pub peer: SocketAddr,
    pub ap: SocketAddr,
    pub _dir: tempfile::TempDir,
}

impl Relay {
    pub async fn start(uplinks: &[SocketAddr]) -> Self {
        let peer = TcpListener::bind("127.0.0.1:0").expect("a peer port");
        let ap = TcpListener::bind("127.0.0.1:0").expect("an access point port");
        Self::on(peer, ap, uplinks).await
    }

    pub async fn on(peer: TcpListener, ap: TcpListener, uplinks: &[SocketAddr]) -> Self {
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

    pub async fn stop(mut self) -> (SocketAddr, SocketAddr) {
        self.node.stop().await.expect("stop the relay");
        (self.peer, self.ap)
    }
}

/// The mailbox with a node of its own, dialling `servers` as a peer.
pub struct Mailbox {
    pub dir: tempfile::TempDir,
    pub public_key: [u8; 64],
    pub token: [u8; 16],
    pub stop: tokio::sync::oneshot::Sender<()>,
    pub task: tokio::task::JoinHandle<Result<(), String>>,
}

impl Mailbox {
    pub async fn start(servers: Vec<String>) -> Self {
        Self::start_in(tempfile::tempdir().expect("mailbox dir"), servers, true).await
    }

    /// The mailbox on `dir`, which may hold one from before: `issue` the
    /// phone's token on the first start only, a restart keeps it.
    pub async fn start_in(dir: tempfile::TempDir, servers: Vec<String>, issue: bool) -> Self {
        let identity =
            loquax::identity::load_or_create(&dir.path().join("identity")).expect("identity");
        let address = loquax::engine::delivery_address(&identity).expect("address");
        let token = [0x42; 16];
        if issue {
            loquax::store::Store::open_in(dir.path(), address)
                .expect("the store")
                .issue_token(&token, "phone", leviculum_std::local_time::unix_now_secs())
                .expect("a token");
        }
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
                // No name: since 1449 a named home archives its owner's
                // name as the first entry, and these cells count texts.
                display_name: Vec::new(),
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
                lends: Vec::new(),
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

    pub fn home(&self) -> DestinationHash {
        DestinationHash::new(loquax::home::destination_hash(&self.public_key).expect("home"))
    }

    /// The `lxmf.delivery` address a sender on the mesh writes to.
    pub fn delivery(&self) -> DestinationHash {
        DestinationHash::new(loquax::home::lxmf_address(&self.public_key).expect("address"))
    }

    pub async fn stop(self) {
        drop(self.stop_keeping().await);
    }

    /// Stop it and keep its state directory for a restart.
    pub async fn stop_keeping(self) -> tempfile::TempDir {
        let _ = self.stop.send(());
        let _ = self.task.await;
        self.dir
    }
}

/// The phone's leaf, dialling `servers`; `ask` says whether it asks for
/// the home destination's path on every up (rule 2), off only for the
/// positive control.
pub struct Phone {
    pub dir: tempfile::TempDir,
    pub device: leviculum_core::Identity,
    pub attached: Attached,
}

impl Phone {
    pub async fn start(servers: Vec<String>, mailbox: &Mailbox, ask: bool) -> Self {
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
pub fn active(attached: &Attached) -> Option<String> {
    attached
        .rendezvous()
        .filter(|state| state.up)
        .map(|state| state.entry)
}

pub async fn wait_until(what: &str, secs: u64, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: not within {secs} s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn next_watch(
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
pub async fn four_steps(phone: &Phone, mailbox: &Mailbox, budget: Duration) {
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

/// The steps after the first join: a watching link, a send that wakes it, a
/// pull. The token is spent, so no second join.
pub async fn four_steps_again(phone: &Phone, mailbox: &Mailbox) {
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

pub fn entry(addr: SocketAddr) -> String {
    format!("{addr}")
}
