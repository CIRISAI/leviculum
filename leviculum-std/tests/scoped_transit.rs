//! Scoped-transit conformance harness — leviculum#48/#51/#52.
//!
//! This file is the executable specification for the closed-overlay relay
//! posture ("proxy for members only"): it was written BEFORE the features it
//! exercises, and the features were built to turn it green. It stays as the
//! permanent conformance suite so the posture can never silently regress.
//!
//! Topology under test:
//!
//! ```text
//!   member A ──(TCP, IFAC key K)──► relay R ◄──(TCP, IFAC key K)── member B
//!                                    │
//!                              public leaf port P
//!                            (no IFAC, transit: false)
//!                                    │
//!                                stranger X
//! ```
//!
//! The behavioral contract, scenario by scenario:
//!
//! - **S1 membership gate**: a stranger speaking to an IFAC'd port is protocol
//!   silence — TCP connects, but every packet lacking the access code is
//!   dropped at the interface. No announce crosses in either direction.
//! - **S2 member transit**: members reach each other THROUGH the relay —
//!   path learned from a relayed announce, link established across two hops,
//!   data delivered. Relay duty for members is real.
//! - **S3/S4 declared leaf is symmetric no-transit** (leviculum#51): the
//!   public port serves traffic terminating at R (a stranger can link to R's
//!   own destination) but carries NO transit in either direction: member
//!   announces never leave through it, stranger announces never enter the
//!   member fabric through it, and no path forms through R between the two
//!   sides. Declared policy — a peer on P can never build a path expecting
//!   transit R won't provide.
//! - **S5/S6 membership-key rotation without a flag-day** (leviculum#52):
//!   the three-phase rotation contract. Phase 1 `ifac_install_next(K2)`:
//!   accept {K, K2}, still send K — stragglers keep full bidirectional
//!   service. Phase 2 `ifac_activate_next()`: send K2, still accept K — a
//!   straggler's OUTBOUND still lands (its packets are accepted); its
//!   inbound degrades until it upgrades (it cannot validate K2 yet).
//!   Phase 3 `ifac_seal_rotation()`: accept K2 only — stragglers are out.
//!   An upgraded member's live links survive every phase (IFAC is per-packet
//!   masking; no session state rides the key).

use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use leviculum_std::{EventReceiver, NodeEvent};

/// Port band disjoint from the bench (61000+) and interop suites.
static PORT_COUNTER: AtomicU16 = AtomicU16::new(59000);

fn next_port() -> u16 {
    loop {
        let candidate = PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if candidate >= 60500 {
            PORT_COUNTER.store(59000, Ordering::Relaxed);
            continue;
        }
        if StdTcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
}

const NETNAME: &str = "ciris-transit";
const KEY1: &str = "harness-key-one";
const KEY2: &str = "harness-key-two";
const IFAC_SIZE: usize = 16;

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

/// A member: leaf node, IFAC'd TCP client into the relay's member port.
async fn member(relay_member_port: u16, key: &str) -> TestNode {
    let addr: SocketAddr = format!("127.0.0.1:{relay_member_port}").parse().unwrap();
    start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client_ifac(addr, Some(NETNAME), key, IFAC_SIZE),
    )
    .await
}

/// The relay: transport ON, IFAC'd member port; optionally a public
/// no-transit leaf port.
async fn relay(member_port: u16, public_port: Option<u16>, key: &str) -> TestNode {
    // The member port disables ingress control: this harness probes with a
    // fresh destination per phase, which the new-interface burst limiter
    // would (correctly) quarantine — ingress control has its own coverage;
    // here it would mask the IFAC/transit/rotation semantics under test.
    let member_cfg = leviculum_std::config::InterfaceConfig {
        name: "Member TCP Server".to_string(),
        interface_type: "TCPServerInterface".to_string(),
        listen_ip: Some("127.0.0.1".to_string()),
        listen_port: Some(member_port),
        networkname: Some(NETNAME.to_string()),
        passphrase: Some(key.to_string()),
        ifac_size: Some(IFAC_SIZE),
        ingress_control: Some(false),
        ..Default::default()
    };
    let mut b = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_interface_config(member_cfg);
    if let Some(p) = public_port {
        let p_addr: SocketAddr = format!("127.0.0.1:{p}").parse().unwrap();
        b = b.add_tcp_server_no_transit(p_addr);
    }
    start(b).await
}

/// A stranger: no IFAC (or, for S1, pointed at the members' port anyway).
async fn stranger(port: u16) -> TestNode {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(addr),
    )
    .await
}

fn make_destination(
    app: &str,
    aspect: &str,
) -> (Destination, [u8; 32], leviculum_core::DestinationHash) {
    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        app,
        &[aspect],
    )
    .expect("destination");
    let hash = *dest.hash();
    (dest, signing_key, hash)
}

/// Drain events until `pred` matches or the deadline passes.
async fn saw_event(
    rx: &mut EventReceiver,
    window: Duration,
    mut pred: impl FnMut(&NodeEvent) -> bool,
) -> bool {
    let dl = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(dl, rx.recv()).await {
            Ok(Some(ev)) => {
                if pred(&ev) {
                    return true;
                }
            }
            Ok(None) | Err(_) => return false,
        }
    }
}

fn announce_of(hash: leviculum_core::DestinationHash) -> impl FnMut(&NodeEvent) -> bool {
    move |ev| {
        matches!(ev, NodeEvent::AnnounceReceived { announce, .. }
            if *announce.destination_hash() == *hash.as_bytes())
    }
}

/// S1 — a stranger on the members' IFAC'd port is protocol silence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s1_stranger_on_member_port_is_protocol_silence() {
    let m_port = next_port();
    let mut r = relay(m_port, None, KEY1).await;
    let mut a = member(m_port, KEY1).await;
    let mut x = stranger(m_port).await; // right port, no access code
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Member announce crosses to the relay; the stranger hears nothing.
    let (dest, _sk, hash) = make_destination("scoped", "s1");
    a.node.register_destination(dest);
    a.node
        .announce_destination(&hash, Some(b"s1"))
        .await
        .expect("announce");

    assert!(
        saw_event(&mut r.rx, Duration::from_secs(5), announce_of(hash)).await,
        "the relay must hear its member's announce"
    );
    assert!(
        !saw_event(&mut x.rx, Duration::from_secs(3), announce_of(hash)).await,
        "a stranger without the access code must hear nothing on an IFAC'd port"
    );

    // And the stranger's own announce never enters the member fabric.
    let (xdest, _xsk, xhash) = make_destination("scoped", "s1x");
    x.node.register_destination(xdest);
    x.node
        .announce_destination(&xhash, Some(b"s1x"))
        .await
        .expect("announce");
    assert!(
        !saw_event(&mut a.rx, Duration::from_secs(3), announce_of(xhash)).await,
        "a stranger's announce must not cross an IFAC'd interface"
    );
}

/// S2 — members get real transit: a two-hop link through the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s2_members_transit_through_the_relay() {
    let m_port = next_port();
    let _r = relay(m_port, None, KEY1).await;
    let mut a = member(m_port, KEY1).await;
    let mut b = member(m_port, KEY1).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let (dest, sk, hash) = make_destination("scoped", "s2");
    a.node.register_destination(dest);
    a.node
        .announce_destination(&hash, Some(b"s2"))
        .await
        .expect("announce");

    assert!(
        saw_event(&mut b.rx, Duration::from_secs(8), announce_of(hash)).await,
        "member B must learn member A's destination through the relay"
    );

    let handle = b
        .node
        .connect(&hash, &sk)
        .await
        .expect("connect through relay");
    let link_id = *handle.link_id();
    let established = b.node.await_link_established(&link_id).await;
    assert!(
        established.is_ok(),
        "link through the relay must establish: {established:?}"
    );

    handle
        .try_send(b"through-the-relay")
        .await
        .expect("send on link");
    assert!(
        saw_event(&mut a.rx, Duration::from_secs(8), |ev| matches!(
            ev,
            NodeEvent::MessageReceived { .. } | NodeEvent::LinkDataReceived { .. }
        ))
        .await,
        "member A must receive data sent through the relay"
    );
}

/// S3 — the declared leaf port serves R itself but leaks nothing outward:
/// member announces must never leave through a `transit: false` interface.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s3_member_announces_never_leave_the_declared_leaf() {
    let m_port = next_port();
    let p_port = next_port();
    let mut r = relay(m_port, Some(p_port), KEY1).await;
    let a = member(m_port, KEY1).await;
    let mut x = stranger(p_port).await; // un-IFAC'd public leaf: X may talk to R
    tokio::time::sleep(Duration::from_millis(600)).await;

    // The leaf port is alive: R's OWN destination is reachable from X.
    let (rdest, rsk, rhash) = make_destination("scoped", "relaylocal");
    r.node.register_destination(rdest);
    r.node
        .announce_destination(&rhash, Some(b"local"))
        .await
        .expect("announce");
    assert!(
        saw_event(&mut x.rx, Duration::from_secs(5), announce_of(rhash)).await,
        "R's own destination must announce on the leaf (leaf-terminating service is real)"
    );
    let xh = x
        .node
        .connect(&rhash, &rsk)
        .await
        .expect("stranger links to R itself");
    assert!(
        x.node.await_link_established(xh.link_id()).await.is_ok(),
        "a stranger may link to the relay's own destination on the leaf port"
    );

    // But a MEMBER's announce must never be rebroadcast out the leaf.
    let (adest, _ask, ahash) = make_destination("scoped", "s3a");
    a.node.register_destination(adest);
    a.node
        .announce_destination(&ahash, Some(b"s3a"))
        .await
        .expect("announce");
    assert!(
        saw_event(&mut r.rx, Duration::from_secs(5), announce_of(ahash)).await,
        "sanity: the relay heard the member announce"
    );
    assert!(
        !saw_event(&mut x.rx, Duration::from_secs(4), announce_of(ahash)).await,
        "leviculum#51: a member announce must not be rebroadcast out a transit:false interface"
    );
}

/// S4 — the symmetric half: a stranger's announce on the leaf must not enter
/// the member fabric, and no path forms through R between the two sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s4_stranger_announces_never_enter_the_member_fabric() {
    let m_port = next_port();
    let p_port = next_port();
    let _r = relay(m_port, Some(p_port), KEY1).await;
    let mut a = member(m_port, KEY1).await;
    let x = stranger(p_port).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let (xdest, _xsk, xhash) = make_destination("scoped", "s4x");
    x.node.register_destination(xdest);
    x.node
        .announce_destination(&xhash, Some(b"s4x"))
        .await
        .expect("announce");

    assert!(
        !saw_event(&mut a.rx, Duration::from_secs(4), announce_of(xhash)).await,
        "leviculum#51: a stranger announce on a transit:false interface must not \
         be rebroadcast into the member fabric"
    );
    assert!(
        !a.node.has_path(&xhash),
        "no path to the stranger may exist in a member's tables"
    );
}

/// Fresh-destination announce probe: does `to` hear a brand-new destination
/// announced by `from` within the window? A fresh destination per probe
/// sidesteps announce dedup/rate-limiting between phases.
async fn announce_probe(from: &mut TestNode, to: &mut TestNode, window_secs: u64) -> bool {
    let (dest, _sk, hash) = make_destination("scoped", "probe");
    from.node.register_destination(dest);
    from.node
        .announce_destination(&hash, Some(b"probe"))
        .await
        .expect("announce");
    saw_event(
        &mut to.rx,
        Duration::from_secs(window_secs),
        announce_of(hash),
    )
    .await
}

/// S5 — the three-phase membership-key rotation contract (leviculum#52),
/// including the straggler semantics the module docs pin: full service in
/// phase 1, outbound-only in phase 2, exclusion after seal, readmission on
/// re-key. No flag-day anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s5_rotation_has_no_flag_day() {
    let m_port = next_port();
    let r = relay(m_port, None, KEY1).await;
    let mut a = member(m_port, KEY1).await;
    let mut b = member(m_port, KEY1).await; // the straggler: never rotates
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Phase 0 — baseline: both directions work.
    assert!(announce_probe(&mut b, &mut a, 8).await, "phase 0: B→A");
    assert!(announce_probe(&mut a, &mut b, 8).await, "phase 0: A→B");

    // Phase 1 — install-next on relay + A (accept both, still send old):
    // the straggler keeps FULL bidirectional service.
    assert!(
        r.node
            .ifac_install_next(Some(NETNAME), KEY2, IFAC_SIZE)
            .unwrap()
            > 0
    );
    assert!(
        a.node
            .ifac_install_next(Some(NETNAME), KEY2, IFAC_SIZE)
            .unwrap()
            > 0
    );
    assert!(
        announce_probe(&mut b, &mut a, 8).await,
        "phase 1: straggler B→A must still work"
    );
    assert!(
        announce_probe(&mut a, &mut b, 8).await,
        "phase 1: A→straggler B must still work"
    );

    // Phase 2 — activate (send new, still accept old): the straggler's
    // OUTBOUND still lands; its inbound is deaf until it re-keys.
    assert!(r.node.ifac_activate_next() > 0);
    assert!(a.node.ifac_activate_next() > 0);
    assert!(
        announce_probe(&mut b, &mut a, 8).await,
        "phase 2: straggler outbound must still land (accept-old window)"
    );
    assert!(
        !announce_probe(&mut a, &mut b, 4).await,
        "phase 2: straggler inbound is deaf (cannot validate the new key)"
    );

    // Phase 3 — seal: the straggler is out entirely.
    assert!(r.node.ifac_seal_rotation() > 0);
    assert!(a.node.ifac_seal_rotation() > 0);
    assert!(
        !announce_probe(&mut b, &mut a, 4).await,
        "phase 3: a sealed window must exclude the straggler"
    );

    // Re-key: a member joining with the new key gets full service.
    let mut b2 = member(m_port, KEY2).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        announce_probe(&mut b2, &mut a, 8).await,
        "re-keyed member must be readmitted"
    );
}

/// S6 — an up-to-date member's live link survives the whole rotation: IFAC
/// is per-packet masking, no session state rides the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s6_live_links_survive_rotation() {
    let m_port = next_port();
    let mut r = relay(m_port, None, KEY1).await;
    let mut a = member(m_port, KEY1).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Establish a link to the relay's own destination before rotating.
    let (rdest, rsk, rhash) = make_destination("scoped", "s6local");
    r.node.register_destination(rdest);
    r.node
        .announce_destination(&rhash, Some(b"s6"))
        .await
        .expect("announce");
    assert!(saw_event(&mut a.rx, Duration::from_secs(8), announce_of(rhash)).await);
    let handle = a.node.connect(&rhash, &rsk).await.expect("connect");
    a.node
        .await_link_established(handle.link_id())
        .await
        .expect("establish before rotation");

    // Full rotation in lockstep.
    assert!(
        r.node
            .ifac_install_next(Some(NETNAME), KEY2, IFAC_SIZE)
            .unwrap()
            > 0
    );
    assert!(
        a.node
            .ifac_install_next(Some(NETNAME), KEY2, IFAC_SIZE)
            .unwrap()
            > 0
    );
    assert!(r.node.ifac_activate_next() > 0);
    assert!(a.node.ifac_activate_next() > 0);
    assert!(r.node.ifac_seal_rotation() > 0);
    assert!(a.node.ifac_seal_rotation() > 0);

    // The pre-rotation link still carries data.
    handle
        .try_send(b"across-the-rotation")
        .await
        .expect("send after seal");
    assert!(
        saw_event(&mut r.rx, Duration::from_secs(8), |ev| matches!(
            ev,
            NodeEvent::MessageReceived { .. } | NodeEvent::LinkDataReceived { .. }
        ))
        .await,
        "a live link must survive a full key rotation"
    );
}
