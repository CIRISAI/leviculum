//! The propagation-node ROLE against genuine Python LXMF clients
//! (Codeberg #384 part 1) — the mirror of `test_lxmf_propagation_node_sync`,
//! which has Python as the node and us as the client.
//!
//! Topology: Python sender (on the hub daemon) ── hub daemon (transport) ──
//! { our propagation node (sans-io NodeCore over TCP), Python recipient
//! daemon }. The sender uploads a PROPAGATED message with our node selected
//! as its outbound propagation node; the recipient drains its mailbox with
//! the real `request_messages_from_propagation_node` — list, fetch, and the
//! confirm-purge round — and the store must be empty afterwards.
//!
//! The event wiring below is the same wiring `lnpnd`'s engine runs in
//! production (lnpnd/src/engine.rs); this test drives it against the sans-io
//! core so the counterpart on the wire is the genuine Python stack.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use rand_core::OsRng;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

use leviculum_core::node::{NodeCore, NodeCoreBuilder, NodeEvent};
use leviculum_core::resource::ResourceStrategy;
use leviculum_core::transport::{Action, InterfaceId, TickOutput};
use leviculum_core::{
    Destination, DestinationHash, DestinationType, Direction, LinkId, MemoryStorage, ProofStrategy,
    RequestError, RequestPolicy,
};
use leviculum_lxmf::{
    GetOutcome, MemoryPropagationStore, PropagationNode, PropagationNodeConfig, PropagationStore,
    StoredMessage, UploadOutcome, MESSAGE_GET_PATH,
};
use leviculum_std::interfaces::hdlc::{DeframeResult, Deframer};

use crate::common::{connect_to_daemon, send_framed, TestClock};
use crate::harness::TestDaemon;

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Our propagation node: the `PropagationNode` role over a sans-io core.
struct PnHost {
    node: NodeCore<OsRng, TestClock, MemoryStorage>,
    role: PropagationNode<MemoryPropagationStore>,
    stream: TcpStream,
    deframer: Deframer,
    destination_hash: DestinationHash,
    pending_proofs: HashMap<LinkId, VecDeque<[u8; 32]>>,
    /// Distinct transient ids the role accepted. A set, not a counter:
    /// a client whose upload proof is late (e.g. under full-suite load)
    /// legally RETRIES the same message, and the role proves the
    /// duplicate too (`UploadOutcome::Accepted`'s contract) — two accept
    /// events for one message are correct protocol behaviour, a second
    /// distinct message is not.
    accepted: std::collections::BTreeSet<leviculum_lxmf::TransientId>,
}

impl PnHost {
    async fn new(daemon: &TestDaemon) -> Self {
        let identity = leviculum_core::identity::Identity::generate(&mut OsRng);
        let mut node =
            NodeCoreBuilder::new().build(OsRng, TestClock, MemoryStorage::with_defaults());
        let mut destination = Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            "lxmf",
            &["propagation"],
        )
        .expect("propagation destination");
        destination.set_accepts_links(true);
        // Proofs are application-driven so the upload proof leaves only
        // after the store append returned ("persist before you prove").
        destination.set_proof_strategy(ProofStrategy::App);
        let destination_hash = *destination.hash();
        node.register_destination(destination);
        node.register_request_handler(destination_hash, MESSAGE_GET_PATH, RequestPolicy::AllowAll);

        let role = PropagationNode::new(
            MemoryPropagationStore::new(64 * 1024),
            PropagationNodeConfig::default(),
        );
        let stream = connect_to_daemon(daemon).await;
        Self {
            node,
            role,
            stream,
            deframer: Deframer::new(),
            destination_hash,
            pending_proofs: HashMap::new(),
            accepted: std::collections::BTreeSet::new(),
        }
    }

    fn owns_link(&self, link_id: &LinkId) -> bool {
        self.node
            .link(link_id)
            .is_some_and(|link| link.destination_hash() == &self.destination_hash)
    }

    async fn announce(&mut self) {
        let app_data = self.role.announce_app_data(unix_secs());
        let output = self
            .node
            .announce_destination(&self.destination_hash, Some(&app_data))
            .expect("announce");
        self.absorb(output).await;
    }

    /// Dispatch a core output's wire actions and feed its events through the
    /// role, exactly as `lnpnd`'s engine does in `on_event`.
    async fn absorb(&mut self, output: TickOutput) {
        let mut queue = vec![output];
        while let Some(output) = queue.pop() {
            for action in &output.actions {
                match action {
                    Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => {
                        send_framed(&mut self.stream, data).await;
                    }
                }
            }
            for event in output.events {
                if let Some(next) = self.handle_event(&event) {
                    queue.push(next);
                }
            }
        }
    }

    fn handle_event(&mut self, event: &NodeEvent) -> Option<TickOutput> {
        match event {
            NodeEvent::LinkEstablished {
                link_id,
                is_initiator: false,
                destination_hash,
                ..
            } if destination_hash == &self.destination_hash => {
                self.node
                    .set_resource_strategy(link_id, ResourceStrategy::AcceptApp)
                    .expect("resource strategy");
                None
            }
            NodeEvent::LinkProofRequested {
                link_id,
                packet_hash,
            } if self.owns_link(link_id) => {
                self.pending_proofs
                    .entry(*link_id)
                    .or_default()
                    .push_back(*packet_hash);
                None
            }
            NodeEvent::LinkDataReceived { link_id, data } if self.owns_link(link_id) => {
                let proof = self
                    .pending_proofs
                    .get_mut(link_id)
                    .and_then(VecDeque::pop_front);
                match self.role.handle_upload(data, unix_secs(), |_, _| {
                    unreachable!("stamp cost 0 must not validate")
                }) {
                    UploadOutcome::Accepted { transient_id, .. } => {
                        self.accepted.insert(transient_id);
                        let packet_hash = proof.expect("a proof decision precedes the data");
                        Some(
                            self.node
                                .send_data_proof(link_id, &packet_hash)
                                .expect("proof send"),
                        )
                    }
                    other => panic!("upload from the Python client refused: {other:?}"),
                }
            }
            NodeEvent::ResourceAdvertised {
                link_id, data_size, ..
            } if self.owns_link(link_id) => {
                let verdict = if self.role.accepts_resource_of(*data_size) {
                    self.node.accept_resource(link_id)
                } else {
                    self.node.reject_resource(link_id)
                };
                Some(verdict.expect("resource verdict"))
            }
            NodeEvent::ResourceCompleted {
                link_id,
                data,
                is_sender: false,
                ..
            } if self.owns_link(link_id) => {
                match self.role.handle_upload(data, unix_secs(), |_, _| {
                    unreachable!("stamp cost 0 must not validate")
                }) {
                    UploadOutcome::Accepted { transient_id, .. } => {
                        self.accepted.insert(transient_id);
                        None
                    }
                    other => panic!("resource upload refused: {other:?}"),
                }
            }
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &self.destination_hash && path == MESSAGE_GET_PATH => {
                let identity = self
                    .node
                    .get_remote_identity(link_id)
                    .cloned()
                    .expect("the Python client identifies before /get");
                let name_hash = Destination::compute_name_hash("lxmf", &["delivery"]);
                let mailbox =
                    *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes();
                let response = match self.role.handle_get(data, &mailbox, unix_secs()) {
                    Ok(GetOutcome::List { response, .. })
                    | Ok(GetOutcome::Fetch { response, .. }) => response,
                    Err(error) => panic!("/get failed: {error:?}"),
                };
                let sent = match self.node.send_response(link_id, request_id, &response) {
                    Ok(output) => output,
                    Err(RequestError::PayloadTooLarge) => {
                        let (_, output) = self
                            .node
                            .send_response_resource(link_id, request_id, &response)
                            .expect("response resource");
                        output
                    }
                    Err(error) => panic!("response failed: {error:?}"),
                };
                Some(sent)
            }
            NodeEvent::LinkClosed { link_id, .. } => {
                self.pending_proofs.remove(link_id);
                None
            }
            _ => None,
        }
    }

    async fn pump_once(&mut self) {
        let mut buf = [0u8; 8192];
        if let Ok(Ok(n)) = timeout(Duration::from_millis(100), self.stream.read(&mut buf)).await {
            assert_ne!(n, 0, "daemon closed the TCP connection");
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
                self.absorb(output).await;
            }
        }
        let output = self.node.handle_timeout();
        self.absorb(output).await;
    }

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
}

/// The acceptance scenario of Codeberg #384 part 1, in-process: a genuine
/// Python client uploads through our node, a second genuine Python client
/// drains it with `/get`, delivery is asserted on the recipient, and the
/// store is empty after the confirmed fetch.
#[tokio::test]
async fn python_clients_upload_and_drain_through_our_propagation_node() {
    let hub = TestDaemon::start().await.expect("start hub daemon");
    let recipient = TestDaemon::start().await.expect("start recipient daemon");
    recipient
        .add_client_interface("127.0.0.1", hub.rns_port(), Some("ToHub"))
        .await
        .expect("connect recipient to hub");
    tokio::time::sleep(Duration::from_secs(1)).await;

    hub.lxmf_init("py-sender", None).await.expect("sender init");
    let recipient_info = recipient
        .lxmf_init("py-recipient", None)
        .await
        .expect("recipient init");
    recipient.lxmf_announce().await.expect("recipient announce");

    let mut host = PnHost::new(&hub).await;
    let pn_hash_hex = hex::encode(host.destination_hash.as_bytes());

    // Announce until both Python clients know us: selection errors until
    // the identity was recalled from our announce.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        host.announce().await;
        host.pump_until(Duration::from_secs(2), |_| false).await;
        let sender_ok = hub.lxmf_set_propagation_node(&pn_hash_hex).await.is_ok();
        let recipient_ok = recipient
            .lxmf_set_propagation_node(&pn_hash_hex)
            .await
            .is_ok();
        if sender_ok && recipient_ok {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Python clients must learn our propagation announce"
        );
    }

    // The sender must also know the recipient's identity to encrypt to it.
    let content = b"stored on the rust propagation node";
    let title = b"pn accept";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let message_hash = loop {
        match hub
            .lxmf_send(
                &recipient_info.delivery_hash,
                "propagated",
                content,
                title,
                None,
            )
            .await
        {
            Ok(hash) => break hash,
            Err(_) if tokio::time::Instant::now() < deadline => {
                host.pump_until(Duration::from_millis(500), |_| false).await;
            }
            Err(e) => panic!("sender could not address the recipient: {e:?}"),
        }
    };

    // The upload lands in our store, and the sender sees SENT: its packet
    // was proven only after the append returned.
    let stored = host
        .pump_until(Duration::from_secs(30), |h| h.role.store().len() == 1)
        .await;
    assert!(stored, "the upload must be appended to our store");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let state = hub
            .lxmf_get_outbound_status(&message_hash)
            .await
            .expect("outbound status");
        if state == "SENT" || state == "DELIVERED" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sender must see the upload proven, last state {state}"
        );
        host.pump_until(Duration::from_millis(300), |_| false).await;
    }

    // The recipient drains its mailbox: list, fetch, confirm-purge.
    recipient
        .lxmf_request_from_propagation_node()
        .await
        .expect("recipient sync");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let (state, last_result) = recipient
            .lxmf_propagation_transfer_state()
            .await
            .expect("transfer state");
        // PR_COMPLETE = 0x07 (LXMRouter.py:72).
        if state == 0x07 && last_result == Some(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recipient sync must complete with one message, state {state:#x} {last_result:?}"
        );
        host.pump_until(Duration::from_millis(300), |_| false).await;
    }

    // Delivered, with content intact and a valid signature against the
    // sender's announced identity.
    let received = recipient.lxmf_get_received().await.expect("received");
    assert_eq!(received.len(), 1, "exactly one delivered message");
    assert_eq!(received[0].content, content);
    assert_eq!(received[0].title, title);

    // The client's explicit confirmation purges the store: empty after the
    // confirmed fetch, and nothing before it deleted anything.
    let emptied = host
        .pump_until(Duration::from_secs(20), |h| h.role.store().is_empty())
        .await;
    assert!(
        emptied,
        "the store must be empty after the confirmed fetch, {} left",
        host.role.store().len()
    );
    assert_eq!(
        host.accepted.len(),
        1,
        "exactly one distinct upload accepted"
    );
}

/// The reference client collects a mailbox our node can only serve in
/// parts — #388's bounded serve, measured against the real
/// `LXMRouter` instead of argued from its source.
///
/// Why this has to be measured: the board's funded serve cap
/// (`HEAP_BUDGET serve_cap=`, `leviculum-nrf/src/heap_census.rs`) is
/// smaller than what a client lists and asks for, so our node answers a
/// 3-message request with 1 message. Nothing in the protocol forbids
/// that — the reference itself skips messages that do not fit its own
/// limit (`reference/LXMF/LXMF/LXMRouter.py:1547`) — but what the CLIENT
/// does with a response shorter than its request is a property of the
/// client, not of the protocol, and only the client can answer it:
///
/// * `message_get_response` iterates whatever arrived
///   (`:1624-1627`), builds `haves` from the messages it actually
///   ingested, and confirms only those (`:1632-1638`);
/// * it then declares the round `PR_COMPLETE` with
///   `propagation_transfer_last_result = len(response)` (`:1640-1643`)
///   — a short answer is a finished round, not a failure and not a
///   retry;
/// * the next round lists again and puts everything it still lacks back
///   into `wants` (`message_list_response`, `:1576-1596`).
///
/// So the unserved are neither lost nor re-requested forever: they are
/// collected on the next round. This test is that sentence, run.
#[tokio::test]
async fn a_python_client_collects_a_capped_mailbox_over_several_rounds() {
    const MESSAGES: usize = 3;

    let hub = TestDaemon::start().await.expect("start hub daemon");
    let recipient = TestDaemon::start().await.expect("start recipient daemon");
    recipient
        .add_client_interface("127.0.0.1", hub.rns_port(), Some("ToHub"))
        .await
        .expect("connect recipient to hub");
    tokio::time::sleep(Duration::from_secs(1)).await;

    hub.lxmf_init("py-sender", None).await.expect("sender init");
    let recipient_info = recipient
        .lxmf_init("py-recipient", None)
        .await
        .expect("recipient init");
    recipient.lxmf_announce().await.expect("recipient announce");

    let mut host = PnHost::new(&hub).await;
    let pn_hash_hex = hex::encode(host.destination_hash.as_bytes());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        host.announce().await;
        host.pump_until(Duration::from_secs(2), |_| false).await;
        let sender_ok = hub.lxmf_set_propagation_node(&pn_hash_hex).await.is_ok();
        let recipient_ok = recipient
            .lxmf_set_propagation_node(&pn_hash_hex)
            .await
            .is_ok();
        if sender_ok && recipient_ok {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Python clients must learn our propagation announce"
        );
    }

    // Three messages into the mailbox.
    for index in 0..MESSAGES {
        let content = format!("capped mailbox message {index}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match hub
                .lxmf_send(
                    &recipient_info.delivery_hash,
                    "propagated",
                    content.as_bytes(),
                    b"pn cap",
                    None,
                )
                .await
            {
                Ok(_) => break,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    host.pump_until(Duration::from_millis(500), |_| false).await;
                }
                Err(e) => panic!("sender could not address the recipient: {e:?}"),
            }
        }
        let stored = host
            .pump_until(Duration::from_secs(30), |h| {
                h.role.store().len() == index + 1
            })
            .await;
        assert!(stored, "upload {index} must be appended to our store");
    }

    // The cap: one message per round, sized from what is actually in the
    // store rather than guessed, in the same accounting the serve loop
    // uses (24 B up front, stored body + 16 B each).
    let mut largest = 0usize;
    host.role
        .store()
        .for_each(&mut |meta: &StoredMessage| {
            largest = largest.max(meta.size as usize);
        })
        .expect("the store can be walked");
    let cap = 24 + largest + 16;
    host.role.set_serve_cap_bytes(Some(cap));

    // Round after round, exactly as the client drives it: each one is
    // PR_COMPLETE, and each one brings what the cap allowed.
    let mut rounds = 0usize;
    let mut received = Vec::new();
    while received.len() < MESSAGES {
        rounds += 1;
        assert!(
            rounds <= MESSAGES + 2,
            "a capped mailbox must drain in rounds, not spin: {} of {MESSAGES} after {rounds}",
            received.len()
        );
        recipient
            .lxmf_request_from_propagation_node()
            .await
            .expect("recipient sync");
        let before = received.len();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            received = recipient.lxmf_get_received().await.expect("received");
            if received.len() > before {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "round {rounds} delivered nothing: still {} of {MESSAGES}",
                received.len()
            );
            host.pump_until(Duration::from_millis(300), |_| false).await;
        }
        // The client calls a short answer a completed sync
        // (LXMRouter.py:1640-1643), not a failure.
        let (state, last_result) = recipient
            .lxmf_propagation_transfer_state()
            .await
            .expect("transfer state");
        assert_eq!(state, 0x07, "PR_COMPLETE after round {rounds}");
        assert_eq!(
            last_result,
            Some(1),
            "the cap funds one message per round, and the client says so"
        );
        // Let the confirm-purge round land before the next list.
        host.pump_until(Duration::from_secs(5), |h| {
            h.role.store().len() == MESSAGES - received.len()
        })
        .await;
    }

    assert!(
        rounds > 1,
        "the point of the test is that one round was not enough"
    );
    assert_eq!(received.len(), MESSAGES, "every message must arrive");
    let mut contents: Vec<String> = received
        .iter()
        .map(|message| String::from_utf8_lossy(&message.content).into_owned())
        .collect();
    contents.sort();
    let expected: Vec<String> = (0..MESSAGES)
        .map(|index| format!("capped mailbox message {index}"))
        .collect();
    assert_eq!(contents, expected, "nothing lost, nothing duplicated");

    let emptied = host
        .pump_until(Duration::from_secs(20), |h| h.role.store().is_empty())
        .await;
    assert!(
        emptied,
        "the store must be empty after the last confirmed fetch, {} left",
        host.role.store().len()
    );
    assert_eq!(
        host.accepted.len(),
        MESSAGES,
        "exactly {MESSAGES} distinct uploads accepted"
    );
}
