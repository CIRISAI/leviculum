//! LXMF end-to-end interop against the real Python LXMF stack
//! (reference/LXMF, pinned 1.1.0).
//!
//! The Rust side drives the sans-io `LxmfRouter` over a live TCP connection
//! to the Python daemon, which runs a real `LXMF.LXMRouter` with a delivery
//! identity. Covered:
//!
//! - opportunistic delivery in both directions,
//! - direct-link delivery in both directions,
//! - a proof-of-work stamp round trip (Python announces a stamp cost, our
//!   stamper satisfies it, Python validates),
//! - a propagation-node sync: our client downloads a message stored for it
//!   on a Python propagation node (the same `enable_propagation()` router
//!   path `lxmd` drives, LXMF/Utilities/lxmd.py:444-452).
//!
//! Assertions cover message content, title, fields, source/destination
//! hashes, timestamps, signature validity and stamps — not just arrival.

use std::time::Duration;

use rand_core::OsRng;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

use leviculum_core::identity::Identity;
use leviculum_core::node::{NodeCore, NodeCoreBuilder};
use leviculum_core::transport::{Action, InterfaceId, TickOutput};
use leviculum_core::{DestinationHash, MemoryStorage};
use leviculum_lxmf::announce;
use leviculum_lxmf::router::{
    LxmfRouter, MessageState, PropagationClientConfig, RouterConfig, RouterEvent, RouterOutput,
};
use leviculum_lxmf::{
    CooperativeStamper, DeliveryMethod, DeliveryStampRequest, LxmfNode, LxmfNodeConfig, Message,
    PropagationTransport, Verification,
};
use leviculum_std::interfaces::hdlc::{DeframeResult, Deframer};

use crate::common::{connect_to_daemon, send_framed, TestClock};
use crate::harness::{LxmfReceived, TestDaemon};

/// Msgpack-encode a byte slice as one bin value.
fn msgpack_bin(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Binary(data.to_vec()))
        .expect("msgpack encode");
    buf
}

/// Msgpack-encode a `{int: bytes}` map (the LXMF fields wire form).
fn msgpack_fields_map(entries: &[(i64, &[u8])]) -> Vec<u8> {
    let map = rmpv::Value::Map(
        entries
            .iter()
            .map(|(k, v)| (rmpv::Value::from(*k), rmpv::Value::Binary(v.to_vec())))
            .collect(),
    );
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &map).expect("msgpack encode");
    buf
}

/// A Rust LXMF client: sans-io `LxmfRouter` over one TCP connection.
struct LxmfClient {
    node: NodeCore<OsRng, TestClock, MemoryStorage>,
    router: LxmfRouter,
    stream: TcpStream,
    deframer: Deframer,
    /// Identity copy for creating outbound messages.
    identity: Identity,
    delivery_hash: [u8; 16],
    events: Vec<RouterEvent>,
    stamp_requests: Vec<DeliveryStampRequest>,
}

impl LxmfClient {
    async fn new(daemon: &TestDaemon) -> Self {
        let identity = Identity::generate(&mut OsRng);
        let identity_copy =
            Identity::from_private_key_bytes(&identity.private_key_bytes().unwrap()).unwrap();
        let identity_hash = *identity.hash();

        let mut node =
            NodeCoreBuilder::new().build(OsRng, TestClock, MemoryStorage::with_defaults());
        let destination =
            LxmfNode::delivery_destination(identity_copy).expect("delivery destination");
        let delivery_hash = *destination.hash().as_bytes();
        let lxmf = LxmfNode::register(&mut node, destination, LxmfNodeConfig::default())
            .expect("LxmfNode::register");
        let router = LxmfRouter::new(lxmf, identity_hash, RouterConfig::default());

        let stream = connect_to_daemon(daemon).await;

        Self {
            node,
            router,
            stream,
            deframer: Deframer::new(),
            identity,
            delivery_hash,
            events: Vec::new(),
            stamp_requests: Vec::new(),
        }
    }

    /// Enable the propagation client (must share the router identity).
    fn enable_propagation_client(&mut self) {
        let identity_copy =
            Identity::from_private_key_bytes(&self.identity.private_key_bytes().unwrap()).unwrap();
        let destination =
            PropagationTransport::destination(identity_copy).expect("propagation destination");
        let transport = PropagationTransport::register(&mut self.node, destination)
            .expect("PropagationTransport::register");
        self.router
            .enable_propagation_client(transport, PropagationClientConfig::default())
            .expect("enable_propagation_client");
    }

    /// Absorb a router output: write wire actions, re-feed spawned events.
    async fn absorb(&mut self, output: RouterOutput) {
        self.events.extend(
            output
                .events
                .iter()
                .filter(|e| !matches!(e, RouterEvent::StampPending(_)))
                .cloned(),
        );
        for event in &output.events {
            if let RouterEvent::StampPending(request) = event {
                self.stamp_requests.push(*request);
            }
        }
        self.absorb_core(output.core).await;
    }

    /// Absorb a core output: write wire actions, route events through the
    /// router (which may emit more of both).
    async fn absorb_core(&mut self, core: TickOutput) {
        let mut queue = vec![core];
        while let Some(output) = queue.pop() {
            for action in &output.actions {
                match action {
                    Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => {
                        send_framed(&mut self.stream, data).await;
                    }
                }
            }
            for event in output.events {
                let routed = self
                    .router
                    .handle_event(&mut self.node, &event)
                    .expect("router handle_event");
                self.events.extend(
                    routed
                        .events
                        .iter()
                        .filter(|e| !matches!(e, RouterEvent::StampPending(_)))
                        .cloned(),
                );
                for ev in &routed.events {
                    if let RouterEvent::StampPending(request) = ev {
                        self.stamp_requests.push(*request);
                    }
                }
                queue.push(routed.core);
            }
        }
    }

    /// One pump cycle: satisfy pending stamp work, read the wire, run timers
    /// and the router tick.
    async fn pump_once(&mut self) {
        while let Some(request) = self.stamp_requests.pop() {
            let mut stamper = CooperativeStamper::cooperative(OsRng);
            let stamp = request
                .generate_with(&mut stamper)
                .await
                .expect("stamp generation");
            let output = self
                .router
                .set_outbound_stamp_result(&self.node, &request, stamp.to_vec())
                .expect("set_outbound_stamp_result");
            self.absorb(output).await;
        }

        let mut buf = [0u8; 8192];
        match timeout(Duration::from_millis(100), self.stream.read(&mut buf)).await {
            Ok(Ok(0)) => panic!("daemon closed the TCP connection"),
            Ok(Ok(n)) => {
                let frames: Vec<Vec<u8>> = self
                    .deframer
                    .process(&buf[..n])
                    .into_iter()
                    .filter_map(|r| match r {
                        DeframeResult::Frame(data) => Some(data),
                        _ => None,
                    })
                    .collect();
                for frame in frames {
                    let output = self.node.handle_packet(InterfaceId(0), &frame);
                    self.absorb_core(output).await;
                }
            }
            _ => {}
        }

        let output = self.node.handle_timeout();
        self.absorb_core(output).await;
        let output = self.router.tick(&mut self.node).expect("router tick");
        self.absorb(output).await;
    }

    /// Pump until `pred` holds or the deadline passes. Returns whether the
    /// predicate was satisfied.
    async fn pump_until<F>(&mut self, duration: Duration, mut pred: F) -> bool
    where
        F: FnMut(&Self) -> bool,
    {
        let deadline = tokio::time::Instant::now() + duration;
        loop {
            if pred(self) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            self.pump_once().await;
        }
    }

    /// Announce our delivery destination with LXMF delivery app data.
    async fn announce(&mut self, display_name: &[u8]) {
        let app_data = announce::delivery(Some(display_name), None);
        let output = self
            .node
            .announce_destination(&DestinationHash::new(self.delivery_hash), Some(&app_data))
            .expect("announce_destination");
        self.absorb_core(output).await;
    }

    /// Create and enqueue an outbound message; returns its message id.
    async fn send(
        &mut self,
        destination: [u8; 16],
        title: &[u8],
        content: &[u8],
        fields: Vec<(i64, Vec<u8>)>,
        method: DeliveryMethod,
    ) -> [u8; 32] {
        // Both the timestamp and the queue's wall-clock state come from the
        // router's own producer (Codeberg #182); this test no longer has a
        // clock of its own to get wrong.
        let message = self
            .router
            .create_message(
                &self.node,
                destination,
                title.to_vec(),
                content.to_vec(),
                fields,
                method,
            )
            .expect("create_message");
        let message_id = message.message_id;
        let output = self.router.enqueue(&self.node, message).expect("enqueue");
        self.absorb(output).await;
        message_id
    }

    fn delivered(&self, message_id: &[u8; 32]) -> bool {
        self.events.iter().any(|e| {
            matches!(
                e,
                RouterEvent::MessageState { message_id: id, state: MessageState::Delivered }
                    if id == message_id
            )
        })
    }

    fn received_message(&self, source: &[u8; 16]) -> Option<&Message> {
        self.events.iter().find_map(|e| match e {
            RouterEvent::MessageReceived(message) if message.source_hash == *source => {
                Some(message.as_ref())
            }
            _ => None,
        })
    }
}

/// Announce our client until the daemon has a path to it (single announces
/// can race the connection; re-drive like wait_for_node_reannounce).
async fn announce_until_daemon_has_path(
    client: &mut LxmfClient,
    daemon: &TestDaemon,
    display_name: &[u8],
    duration: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + duration;
    let hash = client.delivery_hash;
    while tokio::time::Instant::now() < deadline {
        client.announce(display_name).await;
        let announced_at = tokio::time::Instant::now();
        while tokio::time::Instant::now() < announced_at + Duration::from_secs(3) {
            client.pump_once().await;
            if daemon.has_path(hash).await {
                return true;
            }
        }
    }
    false
}

/// Wait until the Python router has delivered a message with this hash.
async fn wait_for_python_received(
    daemon: &TestDaemon,
    message_id: &[u8; 32],
    duration: Duration,
) -> Option<LxmfReceived> {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let messages = daemon.lxmf_get_received().await.unwrap_or_default();
        if let Some(m) = messages
            .iter()
            .find(|m| m.message_hash == hex::encode(message_id))
        {
            return Some(m.clone());
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "wait_for_python_received: {} not in {:?}",
                hex::encode(message_id),
                messages
            );
            return None;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Shared setup: daemon with a Python LXMF client, Rust client with a known
/// path to it, both identities exchanged via announces.
async fn setup_pair(
    stamp_cost: Option<u8>,
) -> (TestDaemon, LxmfClient, crate::harness::LxmfClientInfo) {
    let daemon = TestDaemon::start().await.expect("Failed to start daemon");
    let py_info = daemon
        .lxmf_init("py-peer", stamp_cost)
        .await
        .expect("lxmf_init");

    let mut client = LxmfClient::new(&daemon).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Python announces its delivery destination; we need path + identity +
    // (with a stamp cost) the announced cost cached in the router.
    daemon.lxmf_announce().await.expect("lxmf_announce");
    let py_hash = crate::common::parse_dest_hash(&py_info.delivery_hash);
    let learned = client
        .pump_until(Duration::from_secs(10), |c| c.node.has_path(&py_hash))
        .await;
    assert!(learned, "Rust client should learn the Python LXMF announce");

    // Our announce so Python can recall our identity (signature validation,
    // opportunistic addressing).
    let announced = announce_until_daemon_has_path(
        &mut client,
        &daemon,
        b"rust-client",
        Duration::from_secs(20),
    )
    .await;
    assert!(announced, "daemon should learn the Rust delivery announce");

    (daemon, client, py_info)
}

/// Check the Python-side record of one of our messages against what we sent.
fn assert_python_received(
    received: &LxmfReceived,
    client: &LxmfClient,
    py_delivery_hash: &str,
    title: &[u8],
    content: &[u8],
    fields: &[(i64, &[u8])],
    method: &str,
) {
    assert_eq!(received.content, content, "content must round-trip");
    assert_eq!(received.title, title, "title must round-trip");
    assert_eq!(
        received.source_hash,
        hex::encode(client.delivery_hash),
        "source hash must be our delivery destination"
    );
    assert_eq!(
        received.destination_hash, py_delivery_hash,
        "destination hash must be the Python delivery destination"
    );
    assert!(
        received.signature_validated,
        "Python must validate our message signature (announce known)"
    );
    assert!(received.timestamp > 0.0, "timestamp must be present");
    assert_eq!(received.method, method, "delivery method must match");

    // Python reports umsgpack.packb(message.fields); compare decoded values
    // rather than bytes so msgpack encoding variants cannot false-negative.
    let mut cursor = std::io::Cursor::new(received.fields.as_slice());
    let got = rmpv::decode::read_value(&mut cursor).expect("decode fields");
    let mut expected_cursor = std::io::Cursor::new(msgpack_fields_map(fields));
    let expected = rmpv::decode::read_value(&mut expected_cursor).unwrap();
    assert_eq!(got, expected, "fields must round-trip");
}

/// Opportunistic Rust → Python: single addressed packet, no link.
///
/// Python's delivery destination proves opportunistic packets explicitly
/// (LXMRouter.py:1926-1927 `packet.prove()`), so the delivery must also
/// surface on our side as MessageState::Delivered.
#[tokio::test]
async fn test_lxmf_opportunistic_rust_to_python() {
    let (daemon, mut client, py_info) = setup_pair(None).await;
    let py_hash = crate::common::parse_dest_hash(&py_info.delivery_hash);

    let fields = vec![(5i64, msgpack_bin(b"rust-field-value"))];
    let message_id = client
        .send(
            *py_hash.as_bytes(),
            b"opportunistic title",
            b"hello from rust, opportunistically",
            fields,
            DeliveryMethod::Opportunistic,
        )
        .await;

    let delivered = client
        .pump_until(Duration::from_secs(20), |c| c.delivered(&message_id))
        .await;
    assert!(
        delivered,
        "Python's opportunistic proof must confirm delivery; events: {:?}",
        client.events
    );

    let received = wait_for_python_received(&daemon, &message_id, Duration::from_secs(10))
        .await
        .expect("Python LXMF router must deliver the message");
    assert_python_received(
        &received,
        &client,
        &py_info.delivery_hash,
        b"opportunistic title",
        b"hello from rust, opportunistically",
        &[(5, b"rust-field-value")],
        "opportunistic",
    );
    // No stamp cost announced: Python must see no stamp at all
    // (LXMessage.get_stamp returns None without cost, LXMessage.py:307-309).
    assert_eq!(received.stamp_value, None, "no stamp without a stamp cost");
}

/// Opportunistic Python → Rust: Python LXMRouter sends a single packet to
/// our announced delivery destination; our router must deliver it with a
/// valid signature, and our auto-proof must flip Python's LXMessage state
/// to DELIVERED.
#[tokio::test]
async fn test_lxmf_opportunistic_python_to_rust() {
    let (daemon, mut client, py_info) = setup_pair(None).await;

    let content = b"hello from python, opportunistically";
    let title = b"py title";
    let fields = msgpack_fields_map(&[(7, b"python-field-value")]);
    let message_hash = daemon
        .lxmf_send(
            &hex::encode(client.delivery_hash),
            "opportunistic",
            content,
            title,
            Some(&fields),
        )
        .await
        .expect("lxmf_send");

    let py_hash = crate::common::parse_dest_hash(&py_info.delivery_hash);
    let got = client
        .pump_until(Duration::from_secs(20), |c| {
            c.received_message(py_hash.as_bytes()).is_some()
        })
        .await;
    assert!(got, "our router must deliver the Python message");

    let message = client.received_message(py_hash.as_bytes()).unwrap();
    assert_eq!(message.content, content);
    assert_eq!(message.title, title);
    assert_eq!(message.destination_hash, client.delivery_hash);
    assert_eq!(
        message.verification,
        Verification::Valid,
        "signature must validate against the announced Python identity"
    );
    assert_eq!(message.method, DeliveryMethod::Opportunistic);
    assert_eq!(
        message.fields,
        vec![(7i64, msgpack_bin(b"python-field-value"))],
        "fields must round-trip"
    );
    assert!(message.timestamp > 0.0);
    assert_eq!(hex::encode(message.message_id), message_hash);

    // Our core auto-proves the packet (ProofStrategy::All on the delivery
    // destination) — Python's receipt must flip the message to DELIVERED.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut state = String::new();
    while tokio::time::Instant::now() < deadline {
        state = daemon
            .lxmf_get_outbound_status(&message_hash)
            .await
            .expect("lxmf_get_outbound_status");
        if state == "DELIVERED" {
            break;
        }
        client.pump_once().await;
    }
    assert_eq!(state, "DELIVERED", "our proof must reach Python's receipt");
}

/// Direct-link Rust → Python: the router establishes a link to Python's
/// delivery destination and sends the message over it.
#[tokio::test]
async fn test_lxmf_direct_rust_to_python() {
    let (daemon, mut client, py_info) = setup_pair(None).await;
    let py_hash = crate::common::parse_dest_hash(&py_info.delivery_hash);

    let fields = vec![(11i64, msgpack_bin(b"direct-field"))];
    let message_id = client
        .send(
            *py_hash.as_bytes(),
            b"direct title",
            b"hello from rust over a direct link",
            fields,
            DeliveryMethod::Direct,
        )
        .await;

    let delivered = client
        .pump_until(Duration::from_secs(30), |c| c.delivered(&message_id))
        .await;
    assert!(
        delivered,
        "direct delivery must be confirmed; events: {:?}",
        client.events
    );

    let received = wait_for_python_received(&daemon, &message_id, Duration::from_secs(10))
        .await
        .expect("Python LXMF router must deliver the direct message");
    assert_python_received(
        &received,
        &client,
        &py_info.delivery_hash,
        b"direct title",
        b"hello from rust over a direct link",
        &[(11, b"direct-field")],
        "direct",
    );
}

/// Direct-link Python → Rust: Python's LXMRouter opens a link to our
/// delivery destination and delivers over it; our proof confirms it.
#[tokio::test]
async fn test_lxmf_direct_python_to_rust() {
    let (daemon, mut client, py_info) = setup_pair(None).await;

    let content = b"hello from python over a direct link";
    let title = b"py direct title";
    let fields = msgpack_fields_map(&[(13, b"py-direct-field")]);
    let message_hash = daemon
        .lxmf_send(
            &hex::encode(client.delivery_hash),
            "direct",
            content,
            title,
            Some(&fields),
        )
        .await
        .expect("lxmf_send");

    let py_hash = crate::common::parse_dest_hash(&py_info.delivery_hash);
    let got = client
        .pump_until(Duration::from_secs(30), |c| {
            c.received_message(py_hash.as_bytes()).is_some()
        })
        .await;
    assert!(got, "our router must deliver the direct Python message");

    let message = client.received_message(py_hash.as_bytes()).unwrap();
    assert_eq!(message.content, content);
    assert_eq!(message.title, title);
    assert_eq!(message.verification, Verification::Valid);
    assert_eq!(message.method, DeliveryMethod::Direct);
    assert_eq!(
        message.fields,
        vec![(13i64, msgpack_bin(b"py-direct-field"))]
    );
    assert_eq!(hex::encode(message.message_id), message_hash);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut state = String::new();
    while tokio::time::Instant::now() < deadline {
        state = daemon
            .lxmf_get_outbound_status(&message_hash)
            .await
            .expect("lxmf_get_outbound_status");
        if state == "DELIVERED" {
            break;
        }
        client.pump_once().await;
    }
    assert_eq!(
        state, "DELIVERED",
        "our link-data proof must reach Python's receipt"
    );
}

/// Stamp round trip: Python announces a stamp cost, our router requests
/// stamp work, the CooperativeStamper satisfies it, and Python validates
/// the stamp on delivery (LXMRouter.py:1861-1881).
#[tokio::test]
async fn test_lxmf_stamped_delivery_rust_to_python() {
    const STAMP_COST: u8 = 8;
    let (daemon, mut client, py_info) = setup_pair(Some(STAMP_COST)).await;
    let py_hash = crate::common::parse_dest_hash(&py_info.delivery_hash);

    let message_id = client
        .send(
            *py_hash.as_bytes(),
            b"stamped title",
            b"hello with proof of work",
            vec![],
            DeliveryMethod::Opportunistic,
        )
        .await;

    // Stamp generation at cost 8 is fast; the pump loop performs it when the
    // router emits StampPending.
    let delivered = client
        .pump_until(Duration::from_secs(30), |c| c.delivered(&message_id))
        .await;
    assert!(
        delivered,
        "stamped delivery must be confirmed; events: {:?}",
        client.events
    );

    let received = wait_for_python_received(&daemon, &message_id, Duration::from_secs(10))
        .await
        .expect("Python LXMF router must deliver the stamped message");
    assert_eq!(received.content, b"hello with proof of work");
    assert_eq!(
        received.stamp_valid,
        Some(true),
        "Python must validate our proof-of-work stamp against its announced cost"
    );
    let value = received.stamp_value.expect("a validated stamp has a value");
    assert!(
        value >= STAMP_COST as u64,
        "stamp value {value} must meet the announced cost {STAMP_COST}"
    );
}

/// Propagation-node sync: a Python client sends a PROPAGATED message for us
/// to a Python propagation node (LXMRouter.enable_propagation, the lxmd
/// path); our propagation client syncs and must deliver it.
///
/// Topology: Rust client ── PN daemon (transport) ── Python sender daemon.
#[tokio::test]
async fn test_lxmf_propagation_node_sync() {
    let pn_daemon = TestDaemon::start()
        .await
        .expect("Failed to start PN daemon");
    let sender_daemon = TestDaemon::start()
        .await
        .expect("Failed to start sender daemon");
    sender_daemon
        .add_client_interface("127.0.0.1", pn_daemon.rns_port(), Some("ToPN"))
        .await
        .expect("connect sender to PN");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Propagation node: real LXMRouter with enable_propagation() — the same
    // code path lxmd drives (LXMF/Utilities/lxmd.py:444-452).
    pn_daemon
        .lxmf_init("pn-node", None)
        .await
        .expect("PN lxmf_init");
    let pn_hash_hex = pn_daemon
        .lxmf_enable_propagation()
        .await
        .expect("enable_propagation");
    let pn_hash = crate::common::parse_dest_hash(&pn_hash_hex);

    // Python sender client on the far daemon. Its delivery announce must
    // reach us (via the PN's transport) so the downloaded message's
    // signature can validate against a known identity.
    let sender_info = sender_daemon
        .lxmf_init("py-sender", None)
        .await
        .expect("sender lxmf_init");
    sender_daemon
        .lxmf_announce()
        .await
        .expect("sender lxmf_announce");

    // Rust client connects to the PN daemon and enables its propagation
    // client (same identity as the router, required for /get access).
    let mut client = LxmfClient::new(&pn_daemon).await;
    client.enable_propagation_client();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Our delivery announce must reach the SENDER daemon (via PN transport)
    // so it can encrypt the propagated message to our identity.
    let announced = announce_until_daemon_has_path(
        &mut client,
        &sender_daemon,
        b"rust-client",
        Duration::from_secs(30),
    )
    .await;
    assert!(announced, "sender daemon must learn our delivery announce");

    // PN announces itself; both the sender and our client must learn it.
    // announce_propagation_node defers by NODE_ANNOUNCE_DELAY = 20s
    // (LXMRouter.py:41, 338-347), so the wait window must exceed that.
    pn_daemon
        .lxmf_announce_propagation_node()
        .await
        .expect("announce PN");
    let learned = client
        .pump_until(Duration::from_secs(40), |c| c.node.has_path(&pn_hash))
        .await;
    assert!(learned, "Rust client must learn the PN announce");

    // The sender's delivery announce must also have reached us by now.
    let sender_hash = crate::common::parse_dest_hash(&sender_info.delivery_hash);
    let sender_known = client
        .pump_until(Duration::from_secs(10), |c| c.node.has_path(&sender_hash))
        .await;
    assert!(sender_known, "Rust client must learn the sender announce");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match sender_daemon.lxmf_set_propagation_node(&pn_hash_hex).await {
            Ok(()) => break,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => panic!("sender could not select the PN: {e:?}"),
        }
    }

    // Python sender stores the message on the PN for us.
    let content = b"propagated hello for the rust client";
    let title = b"propagated title";
    let message_hash = sender_daemon
        .lxmf_send(
            &hex::encode(client.delivery_hash),
            "propagated",
            content,
            title,
            None,
        )
        .await
        .expect("propagated lxmf_send");

    // Uploaded to the PN = state SENT on the sender.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut state = String::new();
    while tokio::time::Instant::now() < deadline {
        state = sender_daemon
            .lxmf_get_outbound_status(&message_hash)
            .await
            .expect("outbound status");
        if state == "SENT" || state == "DELIVERED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(
        state == "SENT" || state == "DELIVERED",
        "sender must hand the message to the PN, got {state}"
    );

    // Our client selects the PN and syncs.
    let output = client
        .router
        .set_outbound_propagation_node(&mut client.node, Some(pn_hash))
        .expect("set_outbound_propagation_node");
    client.absorb(output).await;
    let output = client
        .router
        .request_messages_from_propagation_node(&mut client.node, None)
        .expect("request_messages_from_propagation_node");
    client.absorb(output).await;

    let synced = client
        .pump_until(Duration::from_secs(30), |c| {
            c.events.iter().any(|e| {
                matches!(
                    e,
                    RouterEvent::PropagationSyncComplete(result) if result.received >= 1
                )
            })
        })
        .await;
    assert!(
        synced,
        "propagation sync must download our message; events: {:?}",
        client.events
    );

    let message = client
        .received_message(sender_hash.as_bytes())
        .expect("downloaded message must be delivered");
    assert_eq!(message.content, content);
    assert_eq!(message.title, title);
    assert_eq!(message.destination_hash, client.delivery_hash);
    assert_eq!(
        message.verification,
        Verification::Valid,
        "signature must validate against the sender's announced identity"
    );
    assert_eq!(hex::encode(message.message_id), message_hash);
}

/// Codeberg #217: the timestamp Python reads off our message carries
/// sub-second precision, and the reverse direction still verifies.
///
/// `LXMessage.timestamp` is `time.time()` on the reference side
/// (`reference/LXMF/LXMF/LXMessage.py:357`) and is hashed into the message ID
/// on both sides. While we stamped whole seconds, two identical messages sent
/// inside one second were one ID and our router refused the second as a
/// duplicate — a message a Python peer would have sent. The wire form does not
/// change: the timestamp was already a MessagePack float64.
///
/// Two halves, both against the live Python router:
///
/// - forward, the precision itself: four messages, at least one with a
///   non-zero fractional second. Four samples because a message can legally
///   land on a whole millisecond boundary; four landing there in a row is
///   1e-12, not a flake budget.
/// - forward, the defect: two byte-identical messages sent back to back are
///   two messages at Python, with two distinct hashes.
/// - reverse: Python's own fractional timestamp still round-trips into our
///   router with a validating signature and a matching message ID.
#[tokio::test]
async fn test_lxmf_timestamp_subsecond_precision_interop() {
    let (daemon, mut client, py_info) = setup_pair(None).await;
    let py_hash = crate::common::parse_dest_hash(&py_info.delivery_hash);

    // --- forward: precision survives to the Python router -----------------
    let mut fractions = Vec::new();
    for index in 0..4u8 {
        let content = format!("subsecond probe {index}");
        let message_id = client
            .send(
                *py_hash.as_bytes(),
                b"subsecond",
                content.as_bytes(),
                Vec::new(),
                DeliveryMethod::Opportunistic,
            )
            .await;
        // `enqueue` only queues; `tick` inside the pump is what puts the packet
        // on the wire, and nothing else in this test drives the client's loop.
        let delivered = client
            .pump_until(Duration::from_secs(20), |c| c.delivered(&message_id))
            .await;
        assert!(
            delivered,
            "Python must prove subsecond probe {index}; events: {:?}",
            client.events
        );
        let received = wait_for_python_received(&daemon, &message_id, Duration::from_secs(20))
            .await
            .unwrap_or_else(|| panic!("Python must deliver subsecond probe {index}"));
        assert!(received.timestamp > 1_700_000_000.0, "plausible unix time");
        fractions.push(received.timestamp - received.timestamp.floor());
    }
    assert!(
        fractions.iter().any(|fraction| *fraction > 0.0),
        "every timestamp landed on a whole second, so the precision never left \
         our side: {fractions:?}"
    );

    // --- forward: two identical messages are two messages -----------------
    let first_id = client
        .send(
            *py_hash.as_bytes(),
            b"Re: status",
            b"OK",
            Vec::new(),
            DeliveryMethod::Opportunistic,
        )
        .await;
    let second_id = client
        .send(
            *py_hash.as_bytes(),
            b"Re: status",
            b"OK",
            Vec::new(),
            DeliveryMethod::Opportunistic,
        )
        .await;
    assert_ne!(
        first_id, second_id,
        "two identical replies sent back to back must be two message IDs"
    );
    let both = client
        .pump_until(Duration::from_secs(30), |c| {
            c.delivered(&first_id) && c.delivered(&second_id)
        })
        .await;
    assert!(
        both,
        "both identical replies must be delivered; events: {:?}",
        client.events
    );
    for (label, id) in [("first", &first_id), ("second", &second_id)] {
        let received = wait_for_python_received(&daemon, id, Duration::from_secs(20))
            .await
            .unwrap_or_else(|| panic!("Python must deliver the {label} identical reply"));
        assert_eq!(received.content, b"OK");
        assert!(
            received.signature_validated,
            "the {label} reply must validate at Python"
        );
    }

    // --- reverse: Python's fractional timestamp round-trips to us ---------
    let content = b"python subsecond reply";
    let message_hash = daemon
        .lxmf_send(
            &hex::encode(client.delivery_hash),
            "direct",
            content,
            b"py subsecond",
            None,
        )
        .await
        .expect("lxmf_send");
    let got = client
        .pump_until(Duration::from_secs(30), |c| {
            c.received_message(py_hash.as_bytes()).is_some()
        })
        .await;
    assert!(got, "our router must deliver the Python message");

    let message = client.received_message(py_hash.as_bytes()).unwrap();
    assert_eq!(message.content, content);
    assert_eq!(
        message.verification,
        Verification::Valid,
        "the signature covers the timestamp; a mangled one would fail here"
    );
    assert_eq!(
        hex::encode(message.message_id),
        message_hash,
        "our message ID must be the hash Python computed over the same timestamp"
    );
    assert!(message.timestamp > 1_700_000_000.0);
}
