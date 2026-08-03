use super::*;
use alloc::{
    collections::{BTreeSet, VecDeque},
    vec,
};
use core::cell::Cell;

use leviculum_core::{
    storage_types::PathEntry, Action, Destination, DestinationType, Direction, Identity,
    InterfaceId, MemoryStorage, NodeCoreBuilder, NodeEvent, ProofStrategy, RequestError,
    RequestPolicy, ResourceError, TickOutput,
};
use rand_core::OsRng;

use crate::{
    node::{LxmfNodeConfig, APP_NAME, DELIVERY_ASPECT},
    propagation::PropagationNodeAnnounce,
    propagation_client::{
        PropagationRequestKind, PropagationTransportEvent, PropagationUploadFailure,
        PROPAGATION_ASPECT,
    },
    router::RouterConfig,
    LxmfNode, Message, Verification, MESSAGE_GET_PATH,
};

const NOW_UNIX: f64 = 1_700_000_000.0;

struct TestClock(Cell<u64>);

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.get()
    }
}

type TestNode = NodeCore<OsRng, TestClock, MemoryStorage>;

fn node() -> TestNode {
    NodeCoreBuilder::new().build(
        OsRng,
        TestClock(Cell::new(1_000)),
        MemoryStorage::with_defaults(),
    )
}

fn identity(seed: u8) -> Identity {
    let mut private = [0u8; 64];
    for (index, byte) in private.iter_mut().enumerate() {
        *byte = seed.wrapping_add(index as u8);
    }
    Identity::from_private_key_bytes(&private).expect("deterministic identity")
}

fn identity_copy(identity: &Identity) -> Identity {
    Identity::from_private_key_bytes(
        &identity
            .private_key_bytes()
            .expect("test identity has private keys"),
    )
    .expect("identity copy")
}

fn public_identity(identity: &Identity) -> Identity {
    Identity::from_public_key_bytes(&identity.public_key_bytes()).expect("public identity")
}

fn take_packets(actions: Vec<Action>) -> Vec<Vec<u8>> {
    actions
        .into_iter()
        .map(|action| match action {
            Action::SendPacket { data, .. } | Action::Broadcast { data, .. } => data,
        })
        .collect()
}

struct ClientHarness {
    node: TestNode,
    router: LxmfRouter,
    events: Vec<RouterEvent>,
}

impl ClientHarness {
    fn absorb_core(&mut self, core: TickOutput) -> Vec<Vec<u8>> {
        let mut actions = core.actions;
        let mut events: VecDeque<NodeEvent> = core.events.into();
        while let Some(event) = events.pop_front() {
            let follow_up = self
                .router
                .handle_event(&mut self.node, &event, NOW_UNIX)
                .expect("client handles NodeCore event");
            self.events.extend(follow_up.events);
            actions.extend(follow_up.core.actions);
            events.extend(follow_up.core.events);
        }
        take_packets(actions)
    }

    fn absorb_router(&mut self, output: RouterOutput) -> Vec<Vec<u8>> {
        self.events.extend(output.events);
        self.absorb_core(output.core)
    }

    fn receive(&mut self, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut outbound = Vec::new();
        for packet in packets {
            let core = self.node.handle_packet(InterfaceId(0), &packet);
            outbound.extend(self.absorb_core(core));
        }
        outbound
    }
}

struct MailboxServer {
    node: TestNode,
    propagation_destination: DestinationHash,
    mailbox: BTreeSet<TransientId>,
    already_have: TransientId,
    fresh: TransientId,
    fresh_bytes: Vec<u8>,
    identified: Option<[u8; 16]>,
    list_requests: usize,
    download_requests: usize,
    acknowledgement_requests: usize,
    purge_rounds: Vec<Vec<TransientId>>,
    response_used_resource: bool,
}

struct UploadSink {
    node: TestNode,
    uploads: Vec<Vec<u8>>,
}

impl UploadSink {
    fn absorb_core(&mut self, core: TickOutput) -> Vec<Vec<u8>> {
        for event in core.events {
            match event {
                NodeEvent::LinkDataReceived { data, .. }
                | NodeEvent::ResourceCompleted {
                    data,
                    is_sender: false,
                    ..
                } => self.uploads.push(data),
                _ => {}
            }
        }
        take_packets(core.actions)
    }

    fn receive(&mut self, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut outbound = Vec::new();
        for packet in packets {
            let core = self.node.handle_packet(InterfaceId(0), &packet);
            outbound.extend(self.absorb_core(core));
        }
        outbound
    }
}

impl MailboxServer {
    fn respond(&mut self, link_id: LinkId, request_id: [u8; 16], data: &[u8]) -> TickOutput {
        let request = MessageGetRequest::decode(data).expect("valid client /get request");
        let (response, expect_resource) = match (
            request.wants.as_deref(),
            request.haves.as_deref(),
            request.transfer_limit_kb,
        ) {
            (None, None, None) => {
                self.list_requests += 1;
                (
                    MessageListResponse::TransientIds(self.mailbox.iter().copied().collect())
                        .encode()
                        .expect("list response"),
                    false,
                )
            }
            (Some(wants), Some(haves), Some(TransferLimit::Integer(1_000))) => {
                self.download_requests += 1;
                assert_eq!(wants, &[self.fresh]);
                assert_eq!(haves, &[self.already_have]);
                self.purge_rounds.push(haves.to_vec());
                for transient_id in haves {
                    self.mailbox.remove(transient_id);
                }
                // A conforming node normally returns each wanted message
                // once. Repeating it here exercises the client's durable
                // transient-ID duplicate accounting without adding server
                // behaviour to production code.
                (
                    MessageGetResponse::Messages(vec![
                        self.fresh_bytes.clone(),
                        self.fresh_bytes.clone(),
                    ])
                    .encode()
                    .expect("download response"),
                    true,
                )
            }
            (None, Some(haves), None) => {
                self.acknowledgement_requests += 1;
                assert_eq!(haves, &[self.fresh, self.fresh]);
                self.purge_rounds.push(haves.to_vec());
                for transient_id in haves {
                    self.mailbox.remove(transient_id);
                }
                (
                    MessageGetResponse::Messages(Vec::new())
                        .encode()
                        .expect("acknowledgement response"),
                    false,
                )
            }
            unexpected => panic!("unexpected /get phase: {unexpected:?}"),
        };

        match self.node.send_response(&link_id, &request_id, &response) {
            Ok(output) => {
                assert!(!expect_resource, "large mailbox response fit a Link packet");
                output
            }
            Err(RequestError::PayloadTooLarge) => {
                assert!(
                    expect_resource,
                    "small mailbox response required a Resource"
                );
                self.response_used_resource = true;
                self.node
                    .send_response_resource(&link_id, &request_id, &response)
                    .expect("send mailbox response Resource")
                    .1
            }
            Err(error) => panic!("mailbox response failed: {error:?}"),
        }
    }

    fn absorb_core(&mut self, core: TickOutput) -> Vec<Vec<u8>> {
        let mut actions = core.actions;
        let mut events: VecDeque<NodeEvent> = core.events.into();
        while let Some(event) = events.pop_front() {
            match event {
                NodeEvent::LinkIdentified { identity_hash, .. } => {
                    self.identified = Some(identity_hash);
                }
                NodeEvent::RequestReceived {
                    link_id,
                    request_id,
                    path,
                    data,
                    ..
                } => {
                    assert_eq!(path, MESSAGE_GET_PATH);
                    assert!(
                        self.identified.is_some(),
                        "client must identify before its first mailbox request"
                    );
                    let response = self.respond(link_id, request_id, &data);
                    actions.extend(response.actions);
                    events.extend(response.events);
                }
                _ => {}
            }
        }
        take_packets(actions)
    }

    fn receive(&mut self, packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut outbound = Vec::new();
        for packet in packets {
            let core = self.node.handle_packet(InterfaceId(0), &packet);
            outbound.extend(self.absorb_core(core));
        }
        outbound
    }
}

fn pump(
    client: &mut ClientHarness,
    server: &mut MailboxServer,
    mut to_server: Vec<Vec<u8>>,
    mut to_client: Vec<Vec<u8>>,
) {
    for _ in 0..512 {
        if !to_server.is_empty() {
            to_client.extend(server.receive(core::mem::take(&mut to_server)));
        }
        if !to_client.is_empty() {
            to_server.extend(client.receive(core::mem::take(&mut to_client)));
        }
        if to_server.is_empty() && to_client.is_empty() {
            return;
        }
    }
    panic!("propagation client exchange did not quiesce");
}

fn pump_upload(
    client: &mut ClientHarness,
    sink: &mut UploadSink,
    mut to_sink: Vec<Vec<u8>>,
    mut to_client: Vec<Vec<u8>>,
) {
    for _ in 0..512 {
        if !to_sink.is_empty() {
            to_client.extend(sink.receive(core::mem::take(&mut to_sink)));
        }
        if !to_client.is_empty() {
            to_sink.extend(client.receive(core::mem::take(&mut to_client)));
        }
        if to_sink.is_empty() && to_client.is_empty() {
            return;
        }
    }
    panic!("propagation upload did not quiesce");
}

fn propagation_announce() -> Vec<u8> {
    PropagationNodeAnnounce {
        legacy_support: false,
        timebase: 1_700_000_000,
        enabled: true,
        transfer_limit_kb: 256,
        sync_limit_kb: 10_240,
        stamp_cost: 0,
        stamp_cost_flexibility: 3,
        peering_cost: 18,
        metadata: Vec::new(),
    }
    .encode()
    .expect("propagation announce")
}

#[test]
fn propagation_client_requires_the_delivery_identity() {
    let delivery_identity = identity(1);
    let delivery_hash = *delivery_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&delivery_identity)).expect("delivery");
    let propagation = PropagationTransport::destination(identity(2)).expect("propagation client");
    let mut core = node();
    let lxmf = LxmfNode::register(&mut core, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut core, propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, delivery_hash, RouterConfig::default());

    assert_eq!(
        router.enable_propagation_client(propagation, PropagationClientConfig::default()),
        Err(RouterError::IdentityMismatch)
    );
}

#[test]
fn configured_propagation_node_without_a_path_is_selected_and_requested() {
    let client_identity = identity(3);
    let client_identity_hash = *client_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let delivery_hash = *delivery.hash();
    let propagation = PropagationTransport::destination(identity_copy(&client_identity))
        .expect("propagation client");
    let recipient_identity = identity(4);
    let recipient =
        LxmfNode::delivery_destination(identity_copy(&recipient_identity)).expect("recipient");
    let recipient_hash = *recipient.hash();
    let preferred = DestinationHash::new([0x42; 16]);

    let mut core = node();
    core.remember_identity(recipient_hash, public_identity(&recipient_identity));
    let lxmf = LxmfNode::register(&mut core, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut core, propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");

    assert!(!core.has_path(&preferred));
    let _ = router
        .select_outbound_propagation_node(&mut core, Some(preferred))
        .expect("configured node is selectable before path discovery");
    assert_eq!(router.outbound_propagation_node(), Some(preferred));

    let message = Message::create(
        recipient_hash.into_bytes(),
        delivery_hash.into_bytes(),
        &client_identity,
        NOW_UNIX,
        Vec::new(),
        b"discover propagation path".to_vec(),
        Vec::new(),
        DeliveryMethod::Propagated,
    )
    .expect("signed LXMF message");
    let message_id = message.message_id;
    let _ = router
        .enqueue(message, core.now_ms(), NOW_UNIX)
        .expect("queue before propagation path exists");

    let output = router
        .tick(&mut core, NOW_UNIX)
        .expect("request missing propagation path");
    assert!(
        !output.core.actions.is_empty(),
        "a path request must be emitted"
    );
    assert_eq!(
        router
            .outbound()
            .get(&message_id)
            .expect("message remains queued during discovery")
            .attempts,
        1,
    );
}

#[test]
fn orphaned_propagation_metadata_requests_once_then_is_recreated_by_announce() {
    let client_identity = identity(5);
    let client_identity_hash = *client_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let propagation = PropagationTransport::destination(identity_copy(&client_identity))
        .expect("propagation client");
    let mut client_node = node();
    let lxmf = LxmfNode::register(&mut client_node, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut client_node, propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");

    let mut server_node = node();
    let server_destination = Destination::new(
        Some(identity(6)),
        Direction::In,
        DestinationType::Single,
        APP_NAME,
        &[PROPAGATION_ASPECT],
    )
    .expect("server propagation destination");
    let server_hash = *server_destination.hash();
    server_node.register_destination(server_destination);

    assert!(router.restore_known_propagation_node(KnownPropagationNode {
        destination: server_hash,
        announce: PropagationNodeAnnounce::decode(&propagation_announce())
            .expect("propagation metadata"),
    }));
    assert!(client_node
        .storage()
        .get_identity(server_hash.as_bytes())
        .is_none());

    let first = router
        .tick(&mut client_node, NOW_UNIX)
        .expect("request orphan refresh");
    assert!(
        !first.core.actions.is_empty(),
        "orphan emits a path request"
    );
    assert!(router.known_propagation_node(&server_hash).is_none());

    let second = router
        .tick(&mut client_node, NOW_UNIX)
        .expect("orphan is not requested again");
    assert!(second.core.actions.is_empty(), "orphan request is one-shot");

    let mut client = ClientHarness {
        node: client_node,
        router,
        events: Vec::new(),
    };
    let announced = server_node
        .announce_destination(&server_hash, Some(&propagation_announce()))
        .expect("fresh propagation announce");
    client.receive(take_packets(announced.actions));

    assert!(client.router.known_propagation_node(&server_hash).is_some());
    assert!(client
        .node
        .storage()
        .get_identity(server_hash.as_bytes())
        .is_some());
}

#[test]
fn propagation_metadata_without_cached_announce_requests_once_even_with_identity() {
    let client_identity = identity(7);
    let client_identity_hash = *client_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let propagation = PropagationTransport::destination(identity_copy(&client_identity))
        .expect("propagation client");
    let server_identity = identity(8);
    let server_destination = Destination::new(
        Some(identity_copy(&server_identity)),
        Direction::In,
        DestinationType::Single,
        APP_NAME,
        &[PROPAGATION_ASPECT],
    )
    .expect("server propagation destination");
    let server_hash = *server_destination.hash();

    let mut core = node();
    core.remember_identity(server_hash, public_identity(&server_identity));
    let lxmf = LxmfNode::register(&mut core, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut core, propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");
    assert!(router.restore_known_propagation_node(KnownPropagationNode {
        destination: server_hash,
        announce: PropagationNodeAnnounce::decode(&propagation_announce())
            .expect("propagation metadata"),
    }));
    assert!(core
        .storage()
        .get_identity(server_hash.as_bytes())
        .is_some());
    assert!(core
        .storage()
        .get_announce_cache(server_hash.as_bytes())
        .is_none());

    let first = router
        .tick(&mut core, NOW_UNIX)
        .expect("request missing announce");
    assert!(
        !first.core.actions.is_empty(),
        "missing cached announce emits a path request"
    );
    assert!(router.known_propagation_node(&server_hash).is_none());

    let second = router
        .tick(&mut core, NOW_UNIX)
        .expect("removed metadata stays removed");
    assert!(second.core.actions.is_empty(), "request remains one-shot");
}

#[test]
fn incomplete_restored_propagation_path_is_removed_before_recovery_request() {
    let client_identity = identity(13);
    let client_identity_hash = *client_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let delivery_hash = *delivery.hash();
    let propagation = PropagationTransport::destination(identity_copy(&client_identity))
        .expect("propagation client");
    let recipient_identity = identity(14);
    let recipient =
        LxmfNode::delivery_destination(identity_copy(&recipient_identity)).expect("recipient");
    let recipient_hash = *recipient.hash();
    let preferred = DestinationHash::new([0x62; 16]);

    let mut core = node();
    core.remember_identity(recipient_hash, public_identity(&recipient_identity));
    let path_expiry = core.now_ms() + 60_000;
    core.storage_mut().set_path(
        preferred.into_bytes(),
        PathEntry {
            hops: 1,
            expires_ms: path_expiry,
            interface_index: 0,
            random_blobs: Vec::new(),
            next_hop: None,
        },
    );
    let lxmf = LxmfNode::register(&mut core, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut core, propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");
    let _ = router
        .select_outbound_propagation_node(&mut core, Some(preferred))
        .expect("configured node is selectable from its restored path");

    let message = Message::create(
        recipient_hash.into_bytes(),
        delivery_hash.into_bytes(),
        &client_identity,
        NOW_UNIX,
        Vec::new(),
        b"recover propagation metadata".to_vec(),
        Vec::new(),
        DeliveryMethod::Propagated,
    )
    .expect("signed LXMF message");
    let _ = router
        .enqueue(message, core.now_ms(), NOW_UNIX)
        .expect("queue with incomplete restored propagation state");

    assert!(core.has_path(&preferred));
    let output = router
        .tick(&mut core, NOW_UNIX)
        .expect("replace incomplete path and request a fresh announce");

    assert!(!core.has_path(&preferred));
    assert!(
        !output.core.actions.is_empty(),
        "recovery must emit a fresh path request"
    );
}

#[test]
fn configured_inbound_sync_without_a_path_matches_python_path_requested_state() {
    let client_identity = identity(5);
    let client_identity_hash = *client_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let propagation = PropagationTransport::destination(identity_copy(&client_identity))
        .expect("propagation client");
    let preferred = DestinationHash::new([0x52; 16]);

    let mut core = node();
    let lxmf = LxmfNode::register(&mut core, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut core, propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");
    let _ = router
        .select_outbound_propagation_node(&mut core, Some(preferred))
        .expect("select configured propagation node");

    let requested = router
        .request_messages_from_propagation_node(&mut core, None)
        .expect("start mailbox synchronisation without a route");
    assert!(!requested.core.actions.is_empty());
    assert_eq!(
        router.propagation_client_state(),
        Some(PropagationClientState::PathRequested)
    );
}

#[test]
fn malformed_list_fails_without_a_response_received_transition() {
    let client_identity = identity(1);
    let client_identity_hash = *client_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let propagation = PropagationTransport::destination(identity_copy(&client_identity))
        .expect("propagation client");
    let mut core = node();
    let lxmf = LxmfNode::register(&mut core, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut core, propagation)
        .expect("register propagation client");
    let router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    let mut runtime = PropagationRuntime::new(propagation, PropagationClientConfig::default());
    runtime.client.state = PropagationClientState::RequestSent;
    let mut output = RouterOutput::default();

    runtime
        .handle_list_response(&router, &mut core, &[0xc1], &mut output)
        .expect("malformed response is a transfer failure");

    assert_eq!(runtime.client.state, PropagationClientState::TransferFailed);
    assert_eq!(
        output.events,
        vec![RouterEvent::PropagationSyncState(PropagationSyncStatus {
            state: PropagationClientState::TransferFailed,
            progress: 0.0,
            transfer_size: None,
        })]
    );
}

#[test]
fn receiving_progress_emits_updated_sync_status_without_a_state_change() {
    let client_identity = identity(2);
    let client_identity_hash = *client_identity.hash();
    let delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let propagation = PropagationTransport::destination(identity_copy(&client_identity))
        .expect("propagation client");
    let mut core = node();
    let lxmf = LxmfNode::register(&mut core, delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut core, propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    let mut runtime = PropagationRuntime::new(propagation, PropagationClientConfig::default());
    runtime.client.state = PropagationClientState::RequestSent;
    let link_id = LinkId::new([0x43; 16]);
    let mut output = RouterOutput::default();

    for progress in [0.25, 0.5] {
        runtime
            .handle_transport_event(
                &mut router,
                &mut core,
                PropagationTransportEvent::RequestProgress {
                    link_id,
                    kind: PropagationRequestKind::Get,
                    progress,
                    transfer_size: 4_096,
                },
                NOW_UNIX,
                &mut output,
            )
            .expect("handle mailbox progress");
    }

    assert_eq!(
        output.events,
        vec![
            RouterEvent::PropagationSyncState(PropagationSyncStatus {
                state: PropagationClientState::Receiving,
                progress: 0.25,
                transfer_size: Some(4_096),
            }),
            RouterEvent::PropagationSyncState(PropagationSyncStatus {
                state: PropagationClientState::Receiving,
                progress: 0.5,
                transfer_size: Some(4_096),
            }),
        ]
    );
}

#[test]
fn router_encrypts_stamps_uploads_and_marks_a_propagated_message_sent() {
    let client_identity = identity(10);
    let client_identity_hash = *client_identity.hash();
    let client_delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let client_delivery_hash = *client_delivery.hash();
    let client_propagation =
        PropagationTransport::destination(identity_copy(&client_identity)).expect("client PN");

    let recipient_identity = identity(20);
    let recipient_delivery =
        LxmfNode::delivery_destination(identity_copy(&recipient_identity)).expect("recipient");
    let recipient_hash = *recipient_delivery.hash();

    let mut client_node = node();
    client_node.remember_identity(recipient_hash, public_identity(&recipient_identity));
    let lxmf = LxmfNode::register(&mut client_node, client_delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut client_node, client_propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");
    let mut client = ClientHarness {
        node: client_node,
        router,
        events: Vec::new(),
    };

    let mut server_node = node();
    let mut server_destination = Destination::new(
        Some(identity(30)),
        Direction::In,
        DestinationType::Single,
        APP_NAME,
        &[PROPAGATION_ASPECT],
    )
    .expect("server propagation destination");
    server_destination.set_accepts_links(true);
    server_destination.set_proof_strategy(ProofStrategy::All);
    let server_hash = *server_destination.hash();
    server_node.register_destination(server_destination);
    let mut sink = UploadSink {
        node: server_node,
        uploads: Vec::new(),
    };

    let announced = sink
        .node
        .announce_destination(&server_hash, Some(&propagation_announce()))
        .expect("announce propagation node");
    pump_upload(
        &mut client,
        &mut sink,
        Vec::new(),
        take_packets(announced.actions),
    );
    assert!(client.router.known_propagation_node(&server_hash).is_some());

    let selected = client
        .router
        .set_outbound_propagation_node(&mut client.node, Some(server_hash))
        .expect("select propagation node");
    assert!(client.absorb_router(selected).is_empty());

    let message = Message::create(
        recipient_hash.into_bytes(),
        client_delivery_hash.into_bytes(),
        &client_identity,
        NOW_UNIX,
        b"origin upload".to_vec(),
        b"recipient encrypted".to_vec(),
        Vec::new(),
        DeliveryMethod::Propagated,
    )
    .expect("signed LXMF message");
    let message_id = message.message_id;
    let queued = client
        .router
        .enqueue(message, client.node.now_ms(), NOW_UNIX)
        .expect("queue propagated message");
    assert!(client.absorb_router(queued).is_empty());

    let prepared = client
        .router
        .tick(&mut client.node, NOW_UNIX)
        .expect("prepare upload");
    let to_sink = client.absorb_router(prepared);
    let request = client
        .events
        .iter()
        .find_map(|event| match event {
            RouterEvent::PropagationStampPending(request) if request.message_id == message_id => {
                Some(*request)
            }
            _ => None,
        })
        .expect("detached propagation stamp request");
    assert_eq!(request.target_cost, 0);
    pump_upload(&mut client, &mut sink, to_sink, Vec::new());

    // Re-emitting the same detached work lets a host recover after an
    // interrupted calculation, but advancing that derived retry cursor alone
    // must not request another identical durable checkpoint.
    client.router.persistence_dirty = false;
    client.router.next_job_ms = client.node.now_ms();
    client
        .router
        .outbound
        .get_mut(&message_id)
        .expect("queued message")
        .next_attempt_ms = client.node.now_ms();
    let repeated = client
        .router
        .tick(&mut client.node, NOW_UNIX)
        .expect("re-emit detached propagation stamp request");
    assert!(repeated.events.iter().any(|event| matches!(
        event,
        RouterEvent::PropagationStampPending(repeated) if *repeated == request
    )));
    assert!(!repeated
        .events
        .iter()
        .any(|event| matches!(event, RouterEvent::PersistenceRequested)));

    let propagation_stamp = [0x5a; 32];
    let stamped = client
        .router
        .set_outbound_propagation_stamp(&message_id, propagation_stamp, client.node.now_ms())
        .expect("attach propagation stamp");
    assert!(client.absorb_router(stamped).is_empty());
    let attempts_before_link = client
        .router
        .outbound()
        .get(&message_id)
        .expect("queued message")
        .attempts;
    let connecting = client
        .router
        .tick(&mut client.node, NOW_UNIX)
        .expect("establish propagation link");
    assert_eq!(
        client
            .router
            .outbound()
            .get(&message_id)
            .expect("connecting message")
            .attempts,
        attempts_before_link + 1,
        "establishing the propagation link charges exactly one logical attempt"
    );
    let to_sink = client.absorb_router(connecting);
    pump_upload(&mut client, &mut sink, to_sink, Vec::new());

    let attempts_before_submission = client
        .router
        .outbound()
        .get(&message_id)
        .expect("connected message")
        .attempts;
    let submitted = client
        .router
        .tick(&mut client.node, NOW_UNIX)
        .expect("submit upload");
    assert_eq!(
        client
            .router
            .outbound()
            .get(&message_id)
            .expect("submitted message")
            .attempts,
        attempts_before_submission,
        "submitting on an established propagation link is part of the charged attempt"
    );
    let to_sink = client.absorb_router(submitted);
    pump_upload(&mut client, &mut sink, to_sink, Vec::new());

    assert!(!client.router.outbound().contains_key(&message_id));
    assert!(client.events.iter().any(|event| matches!(
        event,
        RouterEvent::MessageState {
            message_id: completed,
            state: super::super::MessageState::Sent,
        } if completed == &message_id
    )));
    assert_eq!(sink.uploads.len(), 1);

    let upload = &sink.uploads[0];
    let mut position = 0;
    assert_eq!(crate::msgpack::array_len(upload, &mut position), Ok(2));
    assert_eq!(
        crate::msgpack::read_f64(upload, &mut position),
        Ok(NOW_UNIX)
    );
    assert_eq!(crate::msgpack::array_len(upload, &mut position), Ok(1));
    let stamped_transient =
        crate::msgpack::read_bin(upload, &mut position).expect("stamped transient bytes");
    assert_eq!(position, upload.len());
    let stamp_offset = stamped_transient.len() - propagation_stamp.len();
    let (unstamped, outer_stamp) = stamped_transient.split_at(stamp_offset);
    assert_eq!(outer_stamp, propagation_stamp);
    assert_eq!(&unstamped[..16], recipient_hash.as_bytes());
    assert_eq!(full_hash(unstamped), request.transient_id);

    let clear_tail = recipient_delivery
        .decrypt(&unstamped[16..])
        .expect("recipient decrypts upload");
    let mut packed = Vec::with_capacity(16 + clear_tail.len());
    packed.extend_from_slice(recipient_hash.as_bytes());
    packed.extend_from_slice(&clear_tail);
    let decoded = Message::unpack(
        &packed,
        None,
        Some(&public_identity(&client_identity)),
        DeliveryMethod::Propagated,
    )
    .expect("recipient decodes clear LXMF message");
    assert_eq!(decoded.message_id, message_id);
    assert_eq!(decoded.verification, Verification::Valid);
    assert_eq!(decoded.content, b"recipient encrypted");
}

fn assert_upload_failure_closes_link(reason: PropagationUploadFailure) {
    let client_identity = identity(40);
    let client_identity_hash = *client_identity.hash();
    let client_delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let client_delivery_hash = *client_delivery.hash();
    let client_propagation =
        PropagationTransport::destination(identity_copy(&client_identity)).expect("client PN");

    let mut client_node = node();
    let lxmf = LxmfNode::register(&mut client_node, client_delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut client_node, client_propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");
    let mut client = ClientHarness {
        node: client_node,
        router,
        events: Vec::new(),
    };

    let mut server_node = node();
    let mut server_destination = Destination::new(
        Some(identity(41)),
        Direction::In,
        DestinationType::Single,
        APP_NAME,
        &[PROPAGATION_ASPECT],
    )
    .expect("server propagation destination");
    server_destination.set_accepts_links(true);
    server_destination.set_proof_strategy(ProofStrategy::All);
    let server_hash = *server_destination.hash();
    server_node.register_destination(server_destination);
    let mut sink = UploadSink {
        node: server_node,
        uploads: Vec::new(),
    };

    let announced = sink
        .node
        .announce_destination(&server_hash, Some(&propagation_announce()))
        .expect("announce propagation node");
    pump_upload(
        &mut client,
        &mut sink,
        Vec::new(),
        take_packets(announced.actions),
    );
    let selected = client
        .router
        .set_outbound_propagation_node(&mut client.node, Some(server_hash))
        .expect("select propagation node");
    assert!(client.absorb_router(selected).is_empty());

    let connecting = client
        .router
        .propagation
        .as_mut()
        .expect("propagation runtime")
        .transport
        .ensure_link(&mut client.node, server_hash)
        .expect("start propagation link");
    let to_sink = client.absorb_core(connecting.core);
    pump_upload(&mut client, &mut sink, to_sink, Vec::new());
    let link_id = client
        .router
        .propagation
        .as_ref()
        .and_then(|runtime| runtime.transport.active_link(&server_hash))
        .expect("established propagation link");
    assert!(client
        .node
        .link(&link_id)
        .is_some_and(|link| link.is_active()));

    let message = Message::create(
        client_delivery_hash.into_bytes(),
        client_delivery_hash.into_bytes(),
        &client_identity,
        NOW_UNIX,
        b"retry teardown".to_vec(),
        Vec::new(),
        Vec::new(),
        DeliveryMethod::Propagated,
    )
    .expect("signed LXMF message");
    let message_id = message.message_id;
    let queued = client
        .router
        .enqueue(message, client.node.now_ms(), NOW_UNIX)
        .expect("queue propagated message");
    assert!(client.absorb_router(queued).is_empty());
    {
        let entry = client
            .router
            .outbound
            .get_mut(&message_id)
            .expect("queued message");
        entry.state = super::super::MessageState::Sending;
        entry.attempts = 1;
        entry.progress = 0.75;
    }

    let mut runtime = client
        .router
        .propagation
        .take()
        .expect("propagation runtime");
    let mut output = RouterOutput::default();
    runtime
        .handle_transport_event(
            &mut client.router,
            &mut client.node,
            PropagationTransportEvent::UploadFailed {
                link_id,
                message_id,
                reason,
            },
            NOW_UNIX,
            &mut output,
        )
        .expect("handle upload failure");
    client.router.propagation = Some(runtime);

    let entry = client
        .router
        .outbound()
        .get(&message_id)
        .expect("message scheduled for retry");
    assert_eq!(entry.state, super::super::MessageState::Outbound);
    assert_eq!(
        entry.attempts, 1,
        "failure does not charge a second attempt"
    );
    assert_eq!(
        entry.next_attempt_ms,
        client
            .node
            .now_ms()
            .saturating_add(super::super::DELIVERY_RETRY_WAIT_MS)
    );
    assert_eq!(entry.progress, 0.01);
    assert!(
        !output.core.actions.is_empty(),
        "failed upload emits a Link close packet before retry"
    );
    assert!(output.core.events.iter().any(
        |event| matches!(event, NodeEvent::LinkClosed { link_id: closed, .. } if *closed == link_id)
    ));
    assert!(client.node.link(&link_id).is_none());
}

#[test]
fn packet_and_resource_upload_failures_close_the_link_before_retry() {
    assert_upload_failure_closes_link(PropagationUploadFailure::PacketTimeout);
    assert_upload_failure_closes_link(PropagationUploadFailure::Resource(ResourceError::Timeout));
}

#[test]
fn in_memory_mailbox_client_discovers_downloads_delivers_and_purges() {
    let client_identity = identity(1);
    let client_identity_hash = *client_identity.hash();
    let client_delivery =
        LxmfNode::delivery_destination(identity_copy(&client_identity)).expect("delivery");
    let client_delivery_hash = *client_delivery.hash();
    let client_propagation =
        PropagationTransport::destination(identity_copy(&client_identity)).expect("client PN");

    let mut client_node = node();
    let lxmf = LxmfNode::register(&mut client_node, client_delivery, LxmfNodeConfig::default())
        .expect("register delivery");
    let propagation = PropagationTransport::register(&mut client_node, client_propagation)
        .expect("register propagation client");
    let mut router = LxmfRouter::new(lxmf, client_identity_hash, RouterConfig::default());
    router
        .enable_propagation_client(propagation, PropagationClientConfig::default())
        .expect("matching propagation identity");

    let source = identity(101);
    let source_hash = Destination::compute_destination_hash(
        &Destination::compute_name_hash(APP_NAME, &[DELIVERY_ASPECT]),
        source.hash(),
    );
    client_node.remember_identity(source_hash, public_identity(&source));

    let mut server_node = node();
    let server_identity = identity(51);
    let mut server_destination = Destination::new(
        Some(server_identity),
        Direction::In,
        DestinationType::Single,
        APP_NAME,
        &[PROPAGATION_ASPECT],
    )
    .expect("server propagation destination");
    server_destination.set_accepts_links(true);
    let server_destination_hash = *server_destination.hash();
    server_node.register_destination(server_destination);
    server_node.register_request_handler(
        server_destination_hash,
        MESSAGE_GET_PATH,
        RequestPolicy::AllowAll,
    );
    server_node.remember_identity(client_delivery_hash, public_identity(&client_identity));

    let content: Vec<u8> = (0..2_048)
        .map(|index| ((index * 73 + 19) & 0xff) as u8)
        .collect();
    let message = Message::create(
        client_delivery_hash.into_bytes(),
        source_hash.into_bytes(),
        &source,
        NOW_UNIX,
        b"propagation e2e".to_vec(),
        content.clone(),
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("signed LXMF message");
    assert!(message.stamp.is_none());
    let packed = message.pack();
    let encrypted = server_node
        .encrypt_for_destination(&client_delivery_hash, &packed[16..])
        .expect("destination encryption");
    let mut propagated = Vec::with_capacity(16 + encrypted.len());
    propagated.extend_from_slice(client_delivery_hash.as_bytes());
    propagated.extend_from_slice(&encrypted);
    let fresh = full_hash(&propagated);
    let already_have = [0x42; 32];

    let mut mailbox = BTreeSet::new();
    mailbox.insert(already_have);
    mailbox.insert(fresh);
    let mut server = MailboxServer {
        node: server_node,
        propagation_destination: server_destination_hash,
        mailbox,
        already_have,
        fresh,
        fresh_bytes: propagated,
        identified: None,
        list_requests: 0,
        download_requests: 0,
        acknowledgement_requests: 0,
        purge_rounds: Vec::new(),
        response_used_resource: false,
    };
    let mut client = ClientHarness {
        node: client_node,
        router,
        events: Vec::new(),
    };
    client
        .router
        .insert_bounded_id(already_have, NOW_UNIX - 1.0, false);
    client
        .router
        .insert_bounded_id(already_have, NOW_UNIX - 1.0, true);

    // Discovery is real NodeCore announce processing, not direct state
    // injection into PropagationTransport.
    let announce = server
        .node
        .announce_destination(
            &server.propagation_destination,
            Some(&propagation_announce()),
        )
        .expect("announce propagation node");
    pump(
        &mut client,
        &mut server,
        Vec::new(),
        take_packets(announce.actions),
    );
    assert!(client.node.has_path(&server.propagation_destination));
    assert!(client
        .router
        .propagation
        .as_ref()
        .and_then(|runtime| runtime
            .transport
            .known_node(&server.propagation_destination))
        .is_some());

    let selected = client
        .router
        .set_outbound_propagation_node(&mut client.node, Some(server.propagation_destination))
        .expect("select propagation node");
    let mut to_server = client.absorb_router(selected);
    let requested = client
        .router
        .request_messages_from_propagation_node(&mut client.node, None)
        .expect("request mailbox");
    to_server.extend(client.absorb_router(requested));
    pump(&mut client, &mut server, to_server, Vec::new());

    assert_eq!(server.identified, Some(client_identity_hash));
    assert_eq!(server.list_requests, 1);
    assert_eq!(server.download_requests, 1);
    assert_eq!(server.acknowledgement_requests, 1);
    assert!(server.response_used_resource);
    assert!(server.mailbox.is_empty());
    assert_eq!(
        server.purge_rounds,
        vec![vec![already_have], vec![fresh, fresh]]
    );

    let delivered: Vec<&Message> = client
        .events
        .iter()
        .filter_map(|event| match event {
            RouterEvent::MessageReceived(message) => Some(message.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].message_id, message.message_id);
    assert_eq!(delivered[0].content, content);
    assert_eq!(delivered[0].verification, Verification::Valid);
    assert_eq!(delivered[0].method, DeliveryMethod::Propagated);
    assert!(client
        .events
        .iter()
        .any(|event| matches!(event, RouterEvent::Duplicate(id) if *id == fresh)));
    assert!(client.events.iter().any(|event| matches!(
        event,
        RouterEvent::PropagationSyncComplete(PropagationSyncResult {
            received: 2,
            duplicates: 1,
        })
    )));
    assert_eq!(
        client.router.propagation_client_state(),
        Some(PropagationClientState::Complete)
    );
    assert!(client
        .router
        .propagation_client_transfer_size()
        .is_some_and(|size| size > 0));
    assert!(client
        .router
        .propagation
        .as_ref()
        .and_then(|runtime| runtime
            .transport
            .active_link(&server.propagation_destination))
        .is_some());
    assert!(client.router.processed_ids.contains_key(&already_have));
    assert!(client.router.processed_ids.contains_key(&fresh));
    assert!(client.router.delivered_ids.contains_key(&already_have));
    assert!(client.router.has_message(&fresh));
    assert!(client
        .router
        .delivered_ids
        .contains_key(&message.message_id));
}
