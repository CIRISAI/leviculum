//! The propagation-node engine: a [`CoreProcessor`] driving
//! [`PropagationNode`] from inside the driver's tick.
//!
//! # Why a `CoreProcessor` and not the public event receiver
//!
//! A client upload arrives as a raw link packet, surfaced as
//! `LinkDataReceived` — an `EventClass::Data` event the public receiver may
//! drop under load (`leviculum-std/src/driver/processor.rs`, "Where the
//! events come from"). A mailbox that silently loses uploads under load
//! would still have proven them if the proof were automatic; instead the
//! propagation destination runs `ProofStrategy::App`, the proof is sent
//! only after the store append returned, and both the data event and the
//! `LinkProofRequested` it must answer ride the tap. This is the same
//! reasoning that put lnmsg's engine on the tap
//! (`docs/src/concepts/lnmsg-architecture.md` §3).
//!
//! # Event order the proof correlation relies on
//!
//! For one inbound link packet the core emits `LinkProofRequested` first
//! and `LinkDataReceived` second, from the same
//! `handle_plain_data_packet` call
//! (`leviculum-core/src/node/link_management.rs:1562-1590`). The engine
//! therefore queues the proof hash per link and pops it when the matching
//! data event is processed; per-link FIFO order is the emission order.

use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use leviculum_core::node::NodeEvent;
use leviculum_core::resource::ResourceStrategy;
use leviculum_core::transport::TickOutput;
use leviculum_core::{
    Destination, DestinationHash, DestinationType, Direction, Identity, LinkId, ProofStrategy,
    RequestError, RequestPolicy,
};
use leviculum_lxmf::constants::WORKBLOCK_EXPAND_ROUNDS_PN;
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::propagation::{MessageListResponse, PeerError};
use leviculum_lxmf::propagation_client::PROPAGATION_ASPECT;
use leviculum_lxmf::{
    CooperativeStamper, Eviction, EvictionReason, GetOutcome, PropagationNode,
    PropagationNodeConfig, PropagationStore, TransientId, UploadOutcome, MESSAGE_GET_PATH,
};

/// The reference announces the propagation destination 20 s after the role
/// comes up (`NODE_ANNOUNCE_DELAY`, `reference/LXMF/LXMF/LXMRouter.py:41`,
/// applied in `announce_propagation_node` `:338-341`).
pub const ANNOUNCE_DELAY_SECS: u64 = 20;

/// Default re-announce cadence: `lxmd`'s default config announces the node
/// every 360 minutes (`announce_interval = 360`,
/// `reference/LXMF/LXMF/Utilities/lxmd.py:981`).
pub const DEFAULT_ANNOUNCE_INTERVAL_SECS: u64 = 360 * 60;

/// Store-maintenance cadence: the reference cleans its message store every
/// `JOB_STORE_INTERVAL × PROCESSING_INTERVAL` = 120 × 4 s
/// (`reference/LXMF/LXMF/LXMRouter.py:871-900`).
const STORE_MAINTENANCE_SECS: u64 = 480;

/// How soon the engine asks the driver back when nothing else is due; the
/// value lnmsg's engine settled on for the same job.
const POLL_INTERVAL_MS: u64 = 200;

/// What the engine needs before the node exists (a processor is installed
/// on the builder, before the node it runs inside).
pub struct EngineConfig<S> {
    pub identity: Identity,
    pub node_config: PropagationNodeConfig,
    pub store: S,
    pub announce_interval_secs: u64,
    /// Delay before the first announce; [`ANNOUNCE_DELAY_SECS`] is the
    /// reference's behaviour and the production value, tests shorten it.
    pub announce_delay_secs: u64,
}

enum State<S> {
    Unregistered(Box<EngineConfig<S>>),
    Ready(Box<Ready<S>>),
    Failed,
}

struct Ready<S> {
    node: PropagationNode<S>,
    destination_hash: DestinationHash,
    /// Whether stamp validation is armed, decided once from the config.
    min_cost: u8,
}

/// One observable engine event, mirrored to the structured log; the channel
/// exists so `main` (and tests) can watch the role without scraping logs.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Ready {
        destination_hash: [u8; 16],
    },
    Broken {
        detail: String,
    },
    Announced,
    Accepted {
        transient_id: TransientId,
        duplicate: bool,
    },
    Rejected {
        detail: String,
    },
    Served {
        form: &'static str,
        count: usize,
    },
    Evicted {
        count: usize,
    },
}

pub struct Engine<S> {
    state: State<S>,
    events: std::sync::mpsc::Sender<EngineEvent>,
    /// Proof hashes awaiting their data event, per link, in emission order.
    pending_proofs: HashMap<LinkId, VecDeque<[u8; 32]>>,
    next_announce_at: Option<u64>,
    announce_interval_secs: u64,
    announce_delay_secs: u64,
    next_maintenance_at: u64,
}

/// Unix seconds, the timestamp domain of the store and the announce.
fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn short_hex(bytes: &[u8]) -> String {
    bytes.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn full_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl<S: PropagationStore> Engine<S> {
    pub fn new(config: EngineConfig<S>) -> (Self, std::sync::mpsc::Receiver<EngineEvent>) {
        let (events, receiver) = std::sync::mpsc::channel();
        let announce_interval_secs = config.announce_interval_secs;
        let announce_delay_secs = config.announce_delay_secs;
        (
            Self {
                state: State::Unregistered(Box::new(config)),
                events,
                pending_proofs: HashMap::new(),
                next_announce_at: None,
                announce_interval_secs,
                announce_delay_secs,
                next_maintenance_at: 0,
            },
            receiver,
        )
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }

    /// The propagation destination hash, once registered.
    pub fn destination_hash(&self) -> Option<[u8; 16]> {
        match &self.state {
            State::Ready(ready) => Some(*ready.destination_hash.as_bytes()),
            _ => None,
        }
    }

    /// Number of stored messages, once registered. For operator surfaces and
    /// the periculum helper's store probe.
    pub fn store_count(&self) -> Option<usize> {
        match &self.state {
            State::Ready(ready) => ready.node.store().count().ok(),
            _ => None,
        }
    }

    /// Register the propagation destination and the `/get` handler, once.
    ///
    /// The destination accepts links and proves link data via the
    /// application (`ProofStrategy::App`): the upload proof is the node's
    /// statement "stored", so it must not leave before the append returns
    /// ("persist before you prove",
    /// `docs/src/concepts/propagation-node-on-a-board.md` §3). The two
    /// request handlers on the reference's destination are `/offer` and
    /// `/get` (`reference/LXMF/LXMF/LXMRouter.py:669-670`); `/offer` is
    /// part 2, and an unregistered path is silently dropped by the stack,
    /// which a keyless would-be peer reads as a failed request.
    fn register_if_needed(&mut self, core: &mut StdNodeCoreRef<'_>) {
        let config = match std::mem::replace(&mut self.state, State::Failed) {
            State::Unregistered(config) => *config,
            other => {
                self.state = other;
                return;
            }
        };
        let EngineConfig {
            identity,
            node_config,
            store,
            announce_interval_secs: _,
            announce_delay_secs: _,
        } = config;
        let mut destination = match Destination::new(
            Some(identity),
            Direction::In,
            DestinationType::Single,
            APP_NAME,
            &[PROPAGATION_ASPECT],
        ) {
            Ok(destination) => destination,
            Err(error) => {
                self.emit(EngineEvent::Broken {
                    detail: format!("propagation destination: {error:?}"),
                });
                return;
            }
        };
        destination.set_accepts_links(true);
        destination.set_proof_strategy(ProofStrategy::App);
        let destination_hash = *destination.hash();
        core.register_destination(destination);
        core.register_request_handler(destination_hash, MESSAGE_GET_PATH, RequestPolicy::AllowAll);

        let node = PropagationNode::new(store, node_config);
        let min_cost = node.min_accepted_cost();
        self.emit(EngineEvent::Ready {
            destination_hash: *destination_hash.as_bytes(),
        });
        self.state = State::Ready(Box::new(Ready {
            node,
            destination_hash,
            min_cost,
        }));
    }

    fn take_ready(&mut self, core: &mut StdNodeCoreRef<'_>) -> Option<Box<Ready<S>>> {
        self.register_if_needed(core);
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Ready(ready) => Some(ready),
            other => {
                self.state = other;
                None
            }
        }
    }

    fn owns_link(core: &StdNodeCoreRef<'_>, ready: &Ready<S>, link_id: &LinkId) -> bool {
        core.link(link_id)
            .is_some_and(|link| link.destination_hash() == &ready.destination_hash)
    }

    /// The delivery destination hash of the identified client: the mailbox
    /// selector, derived exactly as the reference derives it from the
    /// remote identity (`message_get_request`,
    /// `reference/LXMF/LXMF/LXMRouter.py:1487`).
    fn delivery_hash_of(identity: &Identity) -> [u8; 16] {
        let name_hash = Destination::compute_name_hash(APP_NAME, &["delivery"]);
        *Destination::compute_destination_hash(&name_hash, identity.hash()).as_bytes()
    }

    /// One upload envelope, from either carrier. Returns whether it was
    /// accepted (so the packet path can prove it).
    fn ingest(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        link_id: &LinkId,
        data: &[u8],
        via: &'static str,
        out: &mut TickOutput,
    ) -> bool {
        let min_cost = ready.min_cost;
        let outcome = ready
            .node
            .handle_upload(data, unix_secs(), |transient_id, stamp| {
                // Cost above 0 is opt-in configuration. The 1000-round PN
                // workblock (WORKBLOCK_EXPAND_ROUNDS_PN,
                // reference/LXMF/LXMF/LXStamper.py:13) streams through the
                // constant-space validator; ~41 000 SHA-256 compressions is
                // ~10 ms of the core lock and trips PROCESSOR_TICK_BUDGET's
                // report, which is the honest signal until validation moves to
                // a worker (part 2, where batch offers make it mandatory).
                let mut stamper = CooperativeStamper::cooperative(rand_core::OsRng);
                futures::executor::block_on(stamper.validate_stamp(
                    transient_id,
                    stamp,
                    min_cost,
                    WORKBLOCK_EXPAND_ROUNDS_PN,
                ))
                .unwrap_or(None)
            });
        match outcome {
            UploadOutcome::Accepted {
                transient_id,
                destination_hash,
                size,
                stamp_value,
                duplicate,
                evicted,
            } => {
                self.log_evictions(&evicted);
                tracing::debug!(
                    event = "PN_ACCEPT",
                    tid = short_hex(&transient_id),
                    dst = full_hex(&destination_hash),
                    bytes = size,
                    value = stamp_value,
                    dup = duplicate,
                    via = via,
                );
                self.emit(EngineEvent::Accepted {
                    transient_id,
                    duplicate,
                });
                true
            }
            UploadOutcome::InvalidStamp { reject } => {
                // The reference packs [ERROR_INVALID_STAMP] into a raw link
                // packet and tears the link down
                // (reference/LXMF/LXMF/LXMRouter.py:2257-2260).
                if let Ok((_, send)) = core.send_packet_on_link(link_id, &reject) {
                    out.merge(send);
                }
                out.merge(core.close_link(link_id));
                self.emit(EngineEvent::Rejected {
                    detail: "invalid stamp".into(),
                });
                false
            }
            UploadOutcome::PeerSyncForm => {
                // Multi-message without a peering key: not supposed to
                // happen, the reference tears the link down
                // (reference/LXMF/LXMF/LXMRouter.py:2382-2385).
                out.merge(core.close_link(link_id));
                self.emit(EngineEvent::Rejected {
                    detail: "peer sync form without peering support".into(),
                });
                false
            }
            UploadOutcome::Malformed(error) => {
                // The reference logs and ignores
                // (reference/LXMF/LXMF/LXMRouter.py:2262-2264).
                tracing::debug!("lnpnd: undecodable upload ignored: {error}");
                false
            }
            UploadOutcome::StoreFailed(error) => {
                // No proof leaves: the client keeps its retry, which is the
                // honest outcome for a store that cannot hold the message.
                self.emit(EngineEvent::Rejected {
                    detail: format!("store failed: {error}"),
                });
                false
            }
        }
    }

    fn log_evictions(&self, evicted: &[Eviction]) {
        for eviction in evicted {
            tracing::debug!(
                event = "PN_EVICT",
                tid = short_hex(&eviction.transient_id),
                bytes = eviction.size,
                age_s = eviction.age_secs,
                reason = match eviction.reason {
                    EvictionReason::Expired => "expired",
                    EvictionReason::Displaced => "displaced",
                },
            );
        }
        if !evicted.is_empty() {
            self.emit(EngineEvent::Evicted {
                count: evicted.len(),
            });
        }
    }

    fn answer_get(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        data: &[u8],
        out: &mut TickOutput,
    ) {
        let Some(identity) = core.get_remote_identity(link_id).cloned() else {
            // Unidentified link: the reference answers ERROR_NO_IDENTITY
            // (reference/LXMF/LXMF/LXMRouter.py:1483).
            let response = MessageListResponse::Error(PeerError::NoIdentity)
                .encode()
                .unwrap_or_default();
            self.respond(core, link_id, request_id, &response, out);
            return;
        };
        let mailbox = Self::delivery_hash_of(&identity);
        match ready.node.handle_get(data, &mailbox, unix_secs()) {
            Ok(GetOutcome::List { response, count }) => {
                tracing::debug!(
                    event = "PN_GET",
                    dst = full_hex(&mailbox),
                    form = "list",
                    count = count,
                    bytes = 0u64,
                    purged = 0usize,
                );
                self.emit(EngineEvent::Served {
                    form: "list",
                    count,
                });
                self.respond(core, link_id, request_id, &response, out);
            }
            Ok(GetOutcome::Fetch {
                response,
                served,
                served_bytes,
                purged,
            }) => {
                tracing::debug!(
                    event = "PN_GET",
                    dst = full_hex(&mailbox),
                    form = "fetch",
                    count = served.len(),
                    bytes = served_bytes,
                    purged = purged.len(),
                );
                self.emit(EngineEvent::Served {
                    form: "fetch",
                    count: served.len(),
                });
                self.respond(core, link_id, request_id, &response, out);
            }
            Err(error) => {
                // The reference answers a request it could not process with
                // None (reference/LXMF/LXMF/LXMRouter.py:1558-1560); on the
                // wire that is msgpack nil.
                tracing::debug!("lnpnd: /get failed: {error}");
                self.respond(core, link_id, request_id, &[0xC0], out);
            }
        }
    }

    fn respond(
        &self,
        core: &mut StdNodeCoreRef<'_>,
        link_id: &LinkId,
        request_id: &[u8; 16],
        response: &[u8],
        out: &mut TickOutput,
    ) {
        match core.send_response(link_id, request_id, response) {
            Ok(send) => out.merge(send),
            Err(RequestError::PayloadTooLarge) => {
                match core.send_response_resource(link_id, request_id, response) {
                    Ok((_, send)) => out.merge(send),
                    Err(error) => tracing::warn!("lnpnd: response resource failed: {error:?}"),
                }
            }
            Err(error) => tracing::warn!("lnpnd: response failed: {error:?}"),
        }
    }

    fn announce(
        &mut self,
        ready: &mut Ready<S>,
        core: &mut StdNodeCoreRef<'_>,
        out: &mut TickOutput,
    ) {
        let app_data = ready.node.announce_app_data(unix_secs());
        match core.announce_destination(&ready.destination_hash, Some(&app_data)) {
            Ok(send) => {
                out.merge(send);
                self.emit(EngineEvent::Announced);
            }
            Err(error) => tracing::warn!("lnpnd: announce failed: {error:?}"),
        }
    }
}

/// The concrete core type the driver hands its processors.
type StdNodeCoreRef<'a> = leviculum_std::driver::StdNodeCore;

impl<S: PropagationStore + Send + 'static> leviculum_std::driver::CoreProcessor for Engine<S> {
    fn on_event(
        &mut self,
        core: &mut leviculum_std::driver::StdNodeCore,
        event: &NodeEvent,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };
        match event {
            // A client (or future peer) opened a link to us: resources are
            // gated by the application, as the reference gates them
            // (`propagation_link_established` sets ACCEPT_APP,
            // reference/LXMF/LXMF/LXMRouter.py:2188-2193).
            NodeEvent::LinkEstablished {
                link_id,
                is_initiator: false,
                destination_hash,
                ..
            } if destination_hash == &ready.destination_hash => {
                if let Err(error) = core.set_resource_strategy(link_id, ResourceStrategy::AcceptApp)
                {
                    tracing::warn!("lnpnd: resource strategy: {error:?}");
                }
            }
            // Emitted before the matching LinkDataReceived (module docs).
            NodeEvent::LinkProofRequested {
                link_id,
                packet_hash,
            } if Self::owns_link(core, &ready, link_id) => {
                self.pending_proofs
                    .entry(*link_id)
                    .or_default()
                    .push_back(*packet_hash);
            }
            NodeEvent::LinkDataReceived { link_id, data }
                if Self::owns_link(core, &ready, link_id) =>
            {
                let proof = self
                    .pending_proofs
                    .get_mut(link_id)
                    .and_then(VecDeque::pop_front);
                let accepted = self.ingest(&mut ready, core, link_id, data, "packet", &mut out);
                if accepted {
                    if let Some(packet_hash) = proof {
                        // Prove only now that the append returned: the proof
                        // is the "stored" statement (persist before you
                        // prove; packet.prove after storing,
                        // reference/LXMF/LXMF/LXMRouter.py:2255).
                        match core.send_data_proof(link_id, &packet_hash) {
                            Ok(send) => out.merge(send),
                            Err(error) => tracing::warn!("lnpnd: proof failed: {error:?}"),
                        }
                    }
                }
            }
            // An upload too big for one link packet arrives as a resource;
            // refuse before transfer when it exceeds the announced per-sync
            // limit (reference/LXMF/LXMF/LXMRouter.py:2220-2224).
            NodeEvent::ResourceAdvertised {
                link_id, data_size, ..
            } if Self::owns_link(core, &ready, link_id) => {
                let verdict = if ready.node.accepts_resource_of(*data_size) {
                    core.accept_resource(link_id)
                } else {
                    core.reject_resource(link_id)
                };
                match verdict {
                    Ok(send) => out.merge(send),
                    Err(error) => tracing::warn!("lnpnd: resource verdict: {error:?}"),
                }
            }
            NodeEvent::ResourceCompleted {
                link_id,
                data,
                is_sender: false,
                ..
            } if Self::owns_link(core, &ready, link_id) => {
                // The resource protocol has its own acknowledgement; no
                // packet proof exists to send here.
                let _ = self.ingest(&mut ready, core, link_id, data, "resource", &mut out);
            }
            NodeEvent::RequestReceived {
                link_id,
                destination_hash,
                request_id,
                path,
                data,
                ..
            } if destination_hash == &ready.destination_hash && path == MESSAGE_GET_PATH => {
                self.answer_get(&mut ready, core, link_id, request_id, data, &mut out);
            }
            NodeEvent::LinkClosed { link_id, .. } => {
                self.pending_proofs.remove(link_id);
            }
            _ => {}
        }
        self.state = State::Ready(ready);
        out
    }

    fn on_tick(
        &mut self,
        core: &mut leviculum_std::driver::StdNodeCore,
        now_ms: u64,
    ) -> TickOutput {
        let mut out = TickOutput::empty();
        let Some(mut ready) = self.take_ready(core) else {
            return out;
        };

        let next_announce = *self
            .next_announce_at
            .get_or_insert(now_ms + self.announce_delay_secs * 1000);
        if now_ms >= next_announce {
            self.announce(&mut ready, core, &mut out);
            self.next_announce_at = Some(now_ms + self.announce_interval_secs * 1000);
        }

        if now_ms >= self.next_maintenance_at {
            let evicted = ready.node.tick(unix_secs());
            self.log_evictions(&evicted);
            self.next_maintenance_at = now_ms + STORE_MAINTENANCE_SECS * 1000;
        }

        self.state = State::Ready(ready);

        let due = [
            self.next_announce_at.unwrap_or(now_ms + POLL_INTERVAL_MS),
            self.next_maintenance_at,
            now_ms + POLL_INTERVAL_MS,
        ]
        .into_iter()
        .filter(|deadline| *deadline > now_ms)
        .min()
        .unwrap_or(now_ms + POLL_INTERVAL_MS);
        out.next_deadline_ms = Some(match out.next_deadline_ms {
            Some(existing) => existing.min(due),
            None => due,
        });
        out
    }
}
