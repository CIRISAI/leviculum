//! Direct-link upgrade end to end (+ciris, leviculum#70).
//!
//! ```text
//!   A (destination) ──TCP──► T (transport + facilitator) ◄──TCP── B (proposes)
//!        ▲                                                         │
//!        └──────────────── punched UDP, after the upgrade ─────────┘
//! ```
//!
//! Real nodes, real sockets, loopback. There is no NAT on loopback, so the
//! facilitator reports each socket's own address; everything else (probe,
//! signals over the relayed link, punch, the link moving onto the new
//! interface) runs exactly as it would across two NATs that allow a punch.

use std::net::{SocketAddr, TcpListener as StdTcpListener, UdpSocket as StdUdpSocket};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use leviculum_core::direct_link::wire::REJECT_POLICY;
use leviculum_core::direct_link::{Failure, ProbeProtocol};
use leviculum_core::node::DirectLinkPolicy;
use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::direct_link::DirectLinkSettings;
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::{EventReceiver, NodeEvent};

/// Port band of its own, clear of the other suites.
static PORT_COUNTER: AtomicU16 = AtomicU16::new(60600);

fn next_port() -> u16 {
    loop {
        let candidate = PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if candidate >= 60900 {
            PORT_COUNTER.store(60600, Ordering::Relaxed);
            continue;
        }
        if StdTcpListener::bind(("127.0.0.1", candidate)).is_ok()
            && StdUdpSocket::bind(("0.0.0.0", candidate)).is_ok()
        {
            return candidate;
        }
    }
}

struct TestNode {
    node: ReticulumNode,
    rx: EventReceiver,
}

async fn start(builder: ReticulumNodeBuilder) -> TestNode {
    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = builder
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    std::mem::forget(storage);
    node.start().await.expect("start node");
    let rx = node.take_event_receiver().expect("event rx");
    TestNode { node, rx }
}

async fn transport_node(tcp_port: u16, facilitator_port: u16) -> TestNode {
    let cfg = leviculum_std::config::InterfaceConfig {
        name: "Relay TCP Server".to_string(),
        interface_type: "TCPServerInterface".to_string(),
        listen_ip: Some("127.0.0.1".to_string()),
        listen_port: Some(tcp_port),
        ingress_control: Some(false),
        ..Default::default()
    };
    start(
        ReticulumNodeBuilder::new()
            .enable_transport(true)
            .add_interface_config(cfg)
            .direct_link(DirectLinkSettings {
                facilitator_port: Some(facilitator_port),
                ..Default::default()
            }),
    )
    .await
}

async fn leaf(tcp_port: u16, settings: DirectLinkSettings) -> TestNode {
    let addr: SocketAddr = format!("127.0.0.1:{tcp_port}").parse().unwrap();
    start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(addr)
            .direct_link(settings),
    )
    .await
}

fn proposer(facilitator_port: u16) -> DirectLinkSettings {
    DirectLinkSettings {
        facilitator: Some(format!("127.0.0.1:{facilitator_port}")),
        protocol: ProbeProtocol::Rnsp,
        ..Default::default()
    }
}

fn accepter() -> DirectLinkSettings {
    DirectLinkSettings {
        policy: DirectLinkPolicy::AcceptAll,
        ..Default::default()
    }
}

async fn next_event(
    rx: &mut EventReceiver,
    window: Duration,
    mut pred: impl FnMut(&NodeEvent) -> bool,
) -> Option<NodeEvent> {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return Some(ev),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

fn received(text: &'static [u8]) -> impl FnMut(&NodeEvent) -> bool {
    move |ev| matches!(ev, NodeEvent::MessageReceived { data, .. } if data == text)
}

/// T, A and B up; A's destination announced and linked to from B through T.
/// Returns the link id and the handle B sends on.
///
/// `external` names a relay already running elsewhere, as its TCP port and
/// facilitator port; `None` starts a leviculum one.
async fn linked_through_relay(
    a_settings: DirectLinkSettings,
    external: Option<(u16, u16)>,
) -> (
    Option<TestNode>,
    TestNode,
    TestNode,
    leviculum_core::LinkId,
    leviculum_std::driver::LinkHandle,
    leviculum_core::DestinationHash,
) {
    let (t, tcp_port, facilitator_port) = match external {
        Some((tcp_port, facilitator_port)) => (None, tcp_port, facilitator_port),
        None => {
            let tcp_port = next_port();
            let facilitator_port = next_port();
            (
                Some(transport_node(tcp_port, facilitator_port).await),
                tcp_port,
                facilitator_port,
            )
        }
    };
    let mut a = leaf(tcp_port, a_settings).await;
    let mut b = leaf(tcp_port, proposer(facilitator_port)).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "directlink",
        &["e2e"],
    )
    .expect("destination");
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    a.node.register_destination(dest);
    a.node
        .announce_destination(&hash, Some(b"a"))
        .await
        .expect("announce");
    assert!(
        next_event(&mut b.rx, Duration::from_secs(8), |ev| matches!(
            ev,
            NodeEvent::AnnounceReceived { announce, .. }
                if *announce.destination_hash() == *hash.as_bytes()
        ))
        .await
        .is_some(),
        "B learns A's destination through T"
    );
    assert_eq!(
        b.node.hops_to(&hash),
        Some(2),
        "through the relay: two hops"
    );

    let handle = b.node.connect(&hash, &signing_key).await.expect("connect");
    let link_id = *handle.link_id();
    b.node
        .await_link_established(&link_id)
        .await
        .expect("link through the relay");
    handle.try_send(b"relayed").await.expect("send");
    assert!(
        next_event(&mut a.rx, Duration::from_secs(8), received(b"relayed"))
            .await
            .is_some(),
        "the relayed link carries data before any upgrade"
    );
    (t, a, b, link_id, handle, hash)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relayed_link_upgrades_and_outlives_its_relay() {
    let (t, a, b, link_id, handle, hash) = linked_through_relay(accepter(), None).await;
    upgrade_and_drop_relay(t, a, b, link_id, handle, hash).await;
}

/// Interop: the same upgrade with rns-rs (lelloman/rns-rs) as the relay and
/// the facilitator. Start its `rnsd` with `enable_transport = Yes`,
/// `probe_port = <P>` and a `TCPServerInterface` on 127.0.0.1:<T>, then run
/// with `LEVICULUM_INTEROP_RELAY=<T>:<P>`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs an external rns-rs rnsd; see the doc comment"]
async fn interop_rns_rs_relay_and_facilitator() {
    let spec = std::env::var("LEVICULUM_INTEROP_RELAY").expect("LEVICULUM_INTEROP_RELAY=tcp:probe");
    let (tcp, probe) = spec.split_once(':').expect("tcp:probe");
    let external = (tcp.parse().unwrap(), probe.parse().unwrap());
    let (t, a, b, link_id, handle, hash) = linked_through_relay(accepter(), Some(external)).await;
    upgrade_and_drop_relay(t, a, b, link_id, handle, hash).await;
}

async fn upgrade_and_drop_relay(
    t: Option<TestNode>,
    mut a: TestNode,
    mut b: TestNode,
    link_id: leviculum_core::LinkId,
    handle: leviculum_std::driver::LinkHandle,
    hash: leviculum_core::DestinationHash,
) {
    b.node.propose_direct_link(&link_id).await.expect("propose");

    let b_up = next_event(&mut b.rx, Duration::from_secs(20), |ev| {
        matches!(
            ev,
            NodeEvent::DirectLinkEstablished { .. } | NodeEvent::DirectLinkFailed { .. }
        )
    })
    .await;
    assert!(
        matches!(
            b_up,
            Some(NodeEvent::DirectLinkEstablished { proposed: true, .. })
        ),
        "B's upgrade: {b_up:?}"
    );
    let a_up = next_event(&mut a.rx, Duration::from_secs(20), |ev| {
        matches!(
            ev,
            NodeEvent::DirectLinkEstablished { .. } | NodeEvent::DirectLinkFailed { .. }
        )
    })
    .await;
    assert!(
        matches!(
            a_up,
            Some(NodeEvent::DirectLinkEstablished {
                proposed: false,
                ..
            })
        ),
        "A's side: {a_up:?}"
    );
    assert!(b.node.direct_link_interface(&link_id).is_some());

    // The new interface announced A's destination to B: one hop now.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while b.node.hops_to(&hash) != Some(1) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        b.node.hops_to(&hash),
        Some(1),
        "A is one hop from B over the punched path"
    );

    // Take the relay away (ours; an external one is the caller's to stop).
    // The link no longer needs it.
    if let Some(mut t) = t {
        t.node.stop().await.expect("stop relay");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle
        .try_send(b"direct")
        .await
        .expect("send after relay gone");
    assert!(
        next_event(&mut a.rx, Duration::from_secs(8), received(b"direct"))
            .await
            .is_some(),
        "data crosses the punched path with the relay gone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_upgrade_leaves_the_relayed_link_working() {
    // A runs the default policy: refuse.
    let (_t, mut a, mut b, link_id, handle, _hash) =
        linked_through_relay(DirectLinkSettings::default(), None).await;

    b.node.propose_direct_link(&link_id).await.expect("propose");
    let outcome = next_event(&mut b.rx, Duration::from_secs(20), |ev| {
        matches!(
            ev,
            NodeEvent::DirectLinkEstablished { .. } | NodeEvent::DirectLinkFailed { .. }
        )
    })
    .await;
    assert!(
        matches!(
            outcome,
            Some(NodeEvent::DirectLinkFailed {
                failure: Failure::Rejected(REJECT_POLICY),
                proposed: true,
                ..
            })
        ),
        "refused by policy: {outcome:?}"
    );
    assert_eq!(b.node.direct_link_interface(&link_id), None);

    handle.try_send(b"still relayed").await.expect("send");
    assert!(
        next_event(
            &mut a.rx,
            Duration::from_secs(8),
            received(b"still relayed")
        )
        .await
        .is_some(),
        "the link keeps working on the relay after a refusal"
    );
    assert!(
        next_event(&mut a.rx, Duration::from_millis(300), |ev| matches!(
            ev,
            NodeEvent::MessageReceived { msgtype, .. }
                if leviculum_core::direct_link::wire::is_signal_msgtype(*msgtype)
        ))
        .await
        .is_none(),
        "the signal never reaches A's application"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proposing_without_a_facilitator_is_an_error() {
    let tcp_port = next_port();
    let n = leaf(tcp_port, DirectLinkSettings::default()).await;
    let err = n
        .node
        .propose_direct_link(&leviculum_core::LinkId::new([1; 16]))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            leviculum_std::Error::DirectLink(leviculum_core::node::DirectLinkError::NoFacilitator)
        ),
        "{err:?}"
    );
}

/// Interop, mixed pair: an rns-rs node owns the destination and accepts;
/// leviculum links to it through a relay and proposes.
///
/// `LEVICULUM_INTEROP_PEER=<relay tcp>:<facilitator udp>:<dest hex>:<sig pub hex>`.
/// With `LEVICULUM_INTEROP_RELAY_PID` set, the relay is killed once the
/// upgrade is up and a second message must still arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs an external relay and rns-rs peer; see the doc comment"]
async fn interop_rns_rs_peer_accepts_our_proposal() {
    let spec = std::env::var("LEVICULUM_INTEROP_PEER").expect("LEVICULUM_INTEROP_PEER");
    let parts: Vec<&str> = spec.split(':').collect();
    let (tcp, probe) = (parts[0].parse().unwrap(), parts[1].parse::<u16>().unwrap());
    let dest_bytes: [u8; 16] = hex_bytes(parts[2]).try_into().unwrap();
    let sig_pub: [u8; 32] = hex_bytes(parts[3]).try_into().unwrap();
    let hash = leviculum_core::DestinationHash::new(dest_bytes);

    let mut b = leaf(tcp, proposer(probe)).await;
    assert!(
        b.node
            .wait_for_path(&hash, Duration::from_secs(20), Duration::from_secs(3))
            .await
            .expect("path request"),
        "path to the rns-rs destination"
    );
    let handle = b.node.connect(&hash, &sig_pub).await.expect("connect");
    let link_id = *handle.link_id();
    b.node
        .await_link_established(&link_id)
        .await
        .expect("link to rns-rs");
    b.node.propose_direct_link(&link_id).await.expect("propose");
    let outcome = next_event(&mut b.rx, Duration::from_secs(25), |ev| {
        matches!(
            ev,
            NodeEvent::DirectLinkEstablished { .. } | NodeEvent::DirectLinkFailed { .. }
        )
    })
    .await;
    assert!(
        matches!(
            outcome,
            Some(NodeEvent::DirectLinkEstablished { proposed: true, .. })
        ),
        "upgrade with rns-rs: {outcome:?}"
    );
    handle
        .try_send(b"hello-from-leviculum")
        .await
        .expect("send");
    if let Ok(pid) = std::env::var("LEVICULUM_INTEROP_RELAY_PID") {
        std::process::Command::new("kill")
            .arg(pid)
            .status()
            .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        handle
            .try_send(b"relay-gone")
            .await
            .expect("send after relay gone");
    }
    // The rns-rs side prints what it receives; give it time to arrive.
    tokio::time::sleep(Duration::from_secs(3)).await;
}

/// Interop, mixed pair the other way: leviculum owns the destination and
/// accepts; an rns-rs node links to it and proposes.
///
/// `LEVICULUM_INTEROP_TCP=<relay tcp>`; the destination and signing key are
/// written to `LEVICULUM_INTEROP_OUT` for the rns-rs side to pick up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs an external relay and rns-rs peer; see the doc comment"]
async fn interop_rns_rs_peer_proposes_to_us() {
    let tcp: u16 = std::env::var("LEVICULUM_INTEROP_TCP")
        .unwrap()
        .parse()
        .unwrap();
    let out = std::env::var("LEVICULUM_INTEROP_OUT").unwrap();
    let mut a = leaf(tcp, accepter()).await;
    let identity = Identity::generate(&mut rand_core::OsRng);
    let sig_pub: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "directlink",
        &["leviculum"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    a.node.register_destination(dest);
    a.node
        .announce_destination(&hash, Some(b"lev"))
        .await
        .unwrap();
    std::fs::write(
        &out,
        format!("{} {}", to_hex(hash.as_bytes()), to_hex(&sig_pub)),
    )
    .unwrap();

    let up = next_event(&mut a.rx, Duration::from_secs(60), |ev| {
        matches!(
            ev,
            NodeEvent::DirectLinkEstablished { .. } | NodeEvent::DirectLinkFailed { .. }
        )
    })
    .await;
    assert!(
        matches!(
            up,
            Some(NodeEvent::DirectLinkEstablished {
                proposed: false,
                ..
            })
        ),
        "rns-rs proposed to us: {up:?}"
    );
    assert!(
        next_event(
            &mut a.rx,
            Duration::from_secs(30),
            received(b"hello-from-rns-rs")
        )
        .await
        .is_some(),
        "rns-rs's message arrives over the upgraded link"
    );
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
