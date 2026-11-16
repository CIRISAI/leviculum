//! The Reticulum side of `lnmapd`: the `lev.maps` destination, serving
//! the same store directory as the HTTP side with the same piece ids
//! (`docs/src/concepts/map-server.md`, "The request protocol").
//!
//! The node is a shared-instance client of a separately running daemon
//! (`lnsd`, or Python's `rnsd`), the topology `lblogd`'s page node
//! proved: connect over IPC, register one destination with two request
//! handlers, announce, answer. Small answers go out as a single
//! RESPONSE packet; pieces fall back to a response Resource, queued per
//! link because a link carries one outgoing resource at a time.
//!
//! Requests:
//!
//! - `/index` — no request data; the signed index's bytes.
//! - `/piece` — request data a msgpack map `{"piece": "<64 hex>"}`;
//!   the piece's bytes.
//!
//! Unknown ids and unreadable files are dropped silently: the protocol
//! has no 404, the client sees a clean timeout, exactly as a NomadNet
//! page node treats a page it does not serve.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use leviculum_core::resource::ResourceError;
use leviculum_core::{LinkId, RequestError, RequestPolicy};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{
    Destination, DestinationHash, DestinationType, Direction, Error as StdError, EventReceiver,
    Identity, NodeEvent, ProofStrategy, ReticulumNode,
};

use crate::store;

/// Truncated request id length (matches the driver's request API).
const REQUEST_ID_LEN: usize = 16;

/// A queued large response: the request id it answers and the wrapped
/// bytes, waiting for the link's outgoing resource slot.
type PendingResponse = ([u8; REQUEST_ID_LEN], Vec<u8>);

/// Errors from building or running the map node.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// Reading or writing the persistent identity file failed.
    #[error("identity file {path}: {source}")]
    IdentityIo {
        /// The identity file path.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The identity file holds no valid private key, or export failed.
    #[error("identity {path}: {message}")]
    Identity {
        /// The identity file path.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// Building the destination failed.
    #[error("destination: {0}")]
    Destination(String),
    /// A node operation (connect, start, announce) failed.
    #[error("node: {0}")]
    Node(#[from] StdError),
    /// The node was built without an event receiver.
    #[error("node has no event receiver")]
    NoEventReceiver,
}

/// Configuration for [`MapNode::start`].
#[derive(Clone, Debug)]
pub struct MapNodeConfig {
    /// Shared-instance name of the running daemon to serve through.
    pub instance_name: String,
    /// The identity file: the SAME identity signs the index
    /// (`lnmapcut --identity`), so the destination a device pins is the
    /// key its index verification recomputes.
    pub identity_path: PathBuf,
    /// Node storage directory (safe to share with the daemon's; a
    /// client with transport off writes nothing there).
    pub storage_dir: PathBuf,
    /// The store directory served (the same one the HTTP side serves).
    pub store_dir: PathBuf,
    /// Display name announced as plain UTF-8 `app_data`.
    pub display_name: String,
    /// Re-announce cadence.
    pub announce_interval: Duration,
}

/// A running map node: connected, destination and handlers registered.
/// Call [`run`](Self::run) to announce and serve.
pub struct MapNode {
    node: ReticulumNode,
    events: EventReceiver,
    dest_hash: DestinationHash,
    store_dir: PathBuf,
    display_name: String,
    announce_interval: Duration,
    /// Large responses waiting for the link's outgoing resource slot.
    pending: HashMap<LinkId, VecDeque<PendingResponse>>,
}

impl MapNode {
    /// Connect to the shared instance, load the identity, register the
    /// `lev.maps` destination and its two handlers.
    pub async fn start(config: MapNodeConfig) -> Result<Self, NodeError> {
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .connect_to_shared_instance(&config.instance_name)
            .storage_path(config.storage_dir.clone())
            .build_sync()?;
        let events = node
            .take_event_receiver()
            .ok_or(NodeError::NoEventReceiver)?;
        node.start().await?;

        let identity = load_or_generate_identity(&config.identity_path)?;
        let dest = maps_destination(identity)?;
        let dest_hash = *dest.hash();
        node.register_destination(dest);
        node.register_request_handler(dest_hash, "/index", RequestPolicy::AllowAll);
        node.register_request_handler(dest_hash, "/piece", RequestPolicy::AllowAll);

        Ok(MapNode {
            node,
            events,
            dest_hash,
            store_dir: config.store_dir,
            display_name: config.display_name,
            announce_interval: config.announce_interval,
            pending: HashMap::new(),
        })
    }

    /// The node's destination hash: what a device pins as
    /// `map_destination`.
    pub fn destination_hash(&self) -> DestinationHash {
        self.dest_hash
    }

    /// Announce the destination and serve requests until the daemon
    /// connection closes.
    pub async fn run(mut self) -> Result<(), NodeError> {
        self.node
            .announce_destination(&self.dest_hash, Some(self.display_name.as_bytes()))
            .await?;
        let mut announce = tokio::time::interval(self.announce_interval);
        announce.tick().await; // consume the immediate first tick

        loop {
            tokio::select! {
                _ = announce.tick() => {
                    if let Err(e) = self
                        .node
                        .announce_destination(&self.dest_hash, Some(self.display_name.as_bytes()))
                        .await
                    {
                        eprintln!("lnmapd: re-announce failed: {e}");
                    }
                }
                event = self.events.recv() => {
                    let Some(event) = event else {
                        return Ok(());
                    };
                    self.handle_event(event).await;
                }
            }
        }
    }

    async fn handle_event(&mut self, event: NodeEvent) {
        match event {
            NodeEvent::RequestReceived {
                link_id,
                request_id,
                path,
                data,
                ..
            } => {
                let bytes = match path.as_str() {
                    "/index" => std::fs::read(store::index_path(&self.store_dir)).ok(),
                    "/piece" => requested_piece_id(&data)
                        .and_then(|id| std::fs::read(store::piece_path(&self.store_dir, &id)).ok()),
                    _ => None,
                };
                // Unknown path or id: protocol-correct silent drop (no
                // 404 exists; the client sees a clean timeout).
                if let Some(bytes) = bytes {
                    self.respond(link_id, request_id, bytes).await;
                }
            }
            // The outgoing resource slot on this link freed up: send
            // the next queued response.
            NodeEvent::ResourceCompleted {
                link_id,
                is_sender: true,
                ..
            }
            | NodeEvent::ResourceFailed {
                link_id,
                is_sender: true,
                ..
            } => {
                self.drain_pending(&link_id).await;
            }
            NodeEvent::LinkClosed { link_id, .. } => {
                self.pending.remove(&link_id);
            }
            _ => {}
        }
    }

    /// Answer one request: the bytes as a msgpack bin, a single
    /// RESPONSE packet if it fits the link MDU, else a response
    /// Resource, queued per link if one is already in flight.
    async fn respond(&mut self, link_id: LinkId, request_id: [u8; REQUEST_ID_LEN], bytes: Vec<u8>) {
        let Some(wrapped) = msgpack_bin(bytes) else {
            return;
        };
        match self
            .node
            .send_response(&link_id, &request_id, &wrapped)
            .await
        {
            Ok(()) => {}
            Err(StdError::Request(RequestError::PayloadTooLarge)) => {
                self.send_large(link_id, request_id, wrapped).await;
            }
            Err(e) => eprintln!("lnmapd: response failed: {e}"),
        }
    }

    async fn send_large(
        &mut self,
        link_id: LinkId,
        request_id: [u8; REQUEST_ID_LEN],
        wrapped: Vec<u8>,
    ) {
        match self
            .node
            .send_response_resource(&link_id, &request_id, &wrapped)
            .await
        {
            Ok(()) => {}
            // A link serves one outgoing resource at a time; queue
            // until the in-flight transfer completes or fails.
            Err(StdError::Resource(ResourceError::TransferInProgress)) => {
                self.pending
                    .entry(link_id)
                    .or_default()
                    .push_back((request_id, wrapped));
            }
            Err(e) => eprintln!("lnmapd: resource response failed: {e}"),
        }
    }

    async fn drain_pending(&mut self, link_id: &LinkId) {
        let Some(queue) = self.pending.get_mut(link_id) else {
            return;
        };
        let Some((request_id, wrapped)) = queue.pop_front() else {
            self.pending.remove(link_id);
            return;
        };
        if queue.is_empty() {
            self.pending.remove(link_id);
        }
        // send_large re-queues on TransferInProgress, so nothing is lost.
        self.send_large(*link_id, request_id, wrapped).await;
    }
}

/// The piece id a `/piece` request's data names, if it is well-formed:
/// a msgpack map with a `piece` entry holding a valid id. Extra entries
/// are ignored, so a later client can add fields without breaking an
/// old server.
fn requested_piece_id(data: &[u8]) -> Option<String> {
    let value = rmpv::decode::read_value(&mut std::io::Cursor::new(data)).ok()?;
    let rmpv::Value::Map(entries) = value else {
        return None;
    };
    let id = entries
        .iter()
        .find(|(k, _)| k.as_str() == Some("piece"))
        .and_then(|(_, v)| v.as_str())?;
    store::valid_piece_id(id).then(|| id.to_owned())
}

/// `bytes` as one msgpack bin value, the response wire form a page
/// fetcher decodes. Encoding into a Vec cannot fail unless the encoder
/// itself broke; `None` then drops the response like an unknown path.
fn msgpack_bin(bytes: Vec<u8>) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(bytes.len() + 8);
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Binary(bytes)).ok()?;
    Some(buf)
}

/// Build the `lev.maps` destination from the server identity.
fn maps_destination(identity: Identity) -> Result<Destination, NodeError> {
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        store::APP_NAME,
        &[store::ASPECT],
    )
    .map_err(|e| NodeError::Destination(e.to_string()))?;
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    Ok(dest)
}

/// Load the persistent identity from `path`, generating and saving a
/// fresh one on first run. Minted 0600, narrowed on load if an older
/// copy is wider (the same discipline as every identity file).
pub fn load_or_generate_identity(path: &Path) -> Result<Identity, NodeError> {
    let io_err = |source| NodeError::IdentityIo {
        path: path.display().to_string(),
        source,
    };
    if path.exists() {
        let bytes = std::fs::read(path).map_err(io_err)?;
        let identity =
            Identity::from_private_key_bytes(&bytes).map_err(|e| NodeError::Identity {
                path: path.display().to_string(),
                message: format!("bad identity file: {e}"),
            })?;
        leviculum_std::narrow_secret_mode(path).map_err(io_err)?;
        Ok(identity)
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let id = leviculum_std::generate_identity();
        let pk = id.private_key_bytes().map_err(|e| NodeError::Identity {
            path: path.display().to_string(),
            message: format!("key export failed: {e}"),
        })?;
        leviculum_std::atomic_write_secret(path, &pk).map_err(io_err)?;
        Ok(id)
    }
}
