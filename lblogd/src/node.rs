//! The NomadNet page node: serves the blog's Micron pages over Reticulum.
//!
//! The node is a shared-instance client of a separately running `lnsd`
//! daemon (the same topology `lncp --listen` uses): it connects over IPC,
//! registers a `nomadnetwork.node` destination with one request handler per
//! page path, announces the destination under the blog's display name, and
//! answers page requests with the rendered Micron bytes. Small pages go out
//! as a single RESPONSE packet; pages larger than the link MDU fall back to
//! a response Resource.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use leviculum_core::resource::ResourceError;
use leviculum_core::{LinkId, RequestError, RequestPolicy};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{
    Destination, DestinationHash, DestinationType, Direction, Error as StdError, EventReceiver,
    Identity, NodeEvent, ProofStrategy, ReticulumNode,
};

use crate::post::{load_posts_dir, Post, PostError};
use crate::render::{render_index_micron, render_post_micron};

/// Truncated request id length (matches the driver's request/response API).
const REQUEST_ID_LEN: usize = 16;

/// The request path of the blog's index page.
const INDEX_PATH: &str = "/page/index.mu";

/// A queued large response: the request id it answers and the encoded page.
type PendingResponse = ([u8; REQUEST_ID_LEN], Vec<u8>);

/// Errors from building or running the blog node.
#[derive(Debug, Error)]
pub enum NodeError {
    /// Loading the posts directory failed.
    #[error("loading posts: {0}")]
    Posts(#[from] PostError),
    /// Reading or writing the persistent identity file failed.
    #[error("identity file {path}: {source}")]
    IdentityIo {
        /// The identity file path.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The identity file holds no valid private key, or key export failed.
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
    /// Encoding a page as msgpack failed.
    #[error("page encoding: {0}")]
    Encode(String),
    /// A node operation (connect, start, announce) failed.
    #[error("node: {0}")]
    Node(#[from] StdError),
    /// The node was built without an event receiver.
    #[error("node has no event receiver")]
    NoEventReceiver,
}

/// Configuration for [`BlogNode::start`].
#[derive(Clone, Debug)]
pub struct BlogNodeConfig {
    /// Shared-instance name of the running `lnsd` daemon to serve through.
    pub instance_name: String,
    /// Data directory: the persistent identity lives at
    /// `<data_dir>/identities/lblogd`, node storage at `<data_dir>/storage`.
    pub data_dir: PathBuf,
    /// Directory of Markdown posts to serve.
    pub posts_dir: PathBuf,
    /// Display name announced as plain UTF-8 `app_data`, which is what shows
    /// up in NomadNet's Announce Stream and `lnomad --nodes`.
    pub display_name: String,
    /// Re-announce cadence. NomadNet nodes announce on the order of hours;
    /// [`BlogNodeConfig::default_announce_interval`] matches that.
    pub announce_interval: Duration,
}

impl BlogNodeConfig {
    /// The NomadNet-like default re-announce cadence.
    pub fn default_announce_interval() -> Duration {
        Duration::from_secs(6 * 60 * 60)
    }
}

/// A running blog page node: connected to the daemon, destination and page
/// handlers registered. Call [`run`](Self::run) to announce and serve.
pub struct BlogNode {
    node: ReticulumNode,
    events: EventReceiver,
    dest_hash: DestinationHash,
    display_name: String,
    announce_interval: Duration,
    /// Rendered page bytes by request path, each already encoded as the one
    /// msgpack bin value `send_response` expects.
    pages: HashMap<String, Vec<u8>>,
    /// Large responses waiting for the link's outgoing resource slot: a link
    /// carries only one outgoing resource at a time, so concurrent large-page
    /// requests on the same link queue here until the slot frees up.
    pending: HashMap<LinkId, VecDeque<PendingResponse>>,
}

impl BlogNode {
    /// Connect to the shared instance, load the identity and posts, and
    /// register the destination and one request handler per page.
    pub async fn start(config: BlogNodeConfig) -> Result<Self, NodeError> {
        let mut node = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .connect_to_shared_instance(&config.instance_name)
            // Safe to share storage with lnsd: a client with
            // enable_transport(false) writes no paths, announces, or packet
            // hashes to storage. Identity is loaded separately.
            .storage_path(config.data_dir.join("storage"))
            .build_sync()?;
        let events = node
            .take_event_receiver()
            .ok_or(NodeError::NoEventReceiver)?;
        node.start().await?;

        // A blog whose destination hash changes on restart is useless, so the
        // identity persists across runs.
        let identity = load_or_generate_identity(&identity_path(&config.data_dir))?;
        let dest = blog_destination(identity)?;
        let dest_hash = *dest.hash();
        node.register_destination(dest);

        let posts = load_posts_dir(&config.posts_dir)?;
        let pages = build_pages(&posts)?;
        // One handler per exact path: the wire carries only the truncated
        // path hash, so prefix or wildcard registration is impossible by
        // design. Unregistered paths are silently dropped by the stack.
        for path in pages.keys() {
            node.register_request_handler(dest_hash, path, RequestPolicy::AllowAll);
        }

        Ok(BlogNode {
            node,
            events,
            dest_hash,
            display_name: config.display_name,
            announce_interval: config.announce_interval,
            pages,
            pending: HashMap::new(),
        })
    }

    /// The node's destination hash (what a browser dials).
    pub fn destination_hash(&self) -> DestinationHash {
        self.dest_hash
    }

    /// The request paths this node serves, sorted.
    pub fn served_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.pages.keys().cloned().collect();
        paths.sort();
        paths
    }

    /// Announce the destination and serve page requests until the daemon
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
                        eprintln!("lblogd: re-announce failed: {e}");
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
                ..
            } => {
                // Unknown path: protocol-correct silent drop (there is no 404
                // in the protocol; the client sees a clean timeout).
                let Some(bytes) = self.pages.get(&path).cloned() else {
                    return;
                };
                self.respond(link_id, request_id, bytes).await;
            }
            // The outgoing resource slot on this link freed up (a large-page
            // transfer finished or died): send the next queued response.
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

    /// Answer one request: single RESPONSE packet if it fits the link MDU,
    /// else a response Resource, queued per link if one is already in flight.
    async fn respond(&mut self, link_id: LinkId, request_id: [u8; REQUEST_ID_LEN], bytes: Vec<u8>) {
        match self.node.send_response(&link_id, &request_id, &bytes).await {
            Ok(()) => {}
            Err(StdError::Request(RequestError::PayloadTooLarge)) => {
                self.send_large(link_id, request_id, bytes).await;
            }
            Err(e) => eprintln!("lblogd: response failed: {e}"),
        }
    }

    async fn send_large(
        &mut self,
        link_id: LinkId,
        request_id: [u8; REQUEST_ID_LEN],
        bytes: Vec<u8>,
    ) {
        match self
            .node
            .send_response_resource(&link_id, &request_id, &bytes)
            .await
        {
            Ok(()) => {}
            // A link serves one outgoing resource at a time; queue until the
            // in-flight transfer completes or fails.
            Err(StdError::Resource(ResourceError::TransferInProgress)) => {
                self.pending
                    .entry(link_id)
                    .or_default()
                    .push_back((request_id, bytes));
            }
            Err(e) => eprintln!("lblogd: resource response failed: {e}"),
        }
    }

    async fn drain_pending(&mut self, link_id: &LinkId) {
        let Some(queue) = self.pending.get_mut(link_id) else {
            return;
        };
        let Some((request_id, bytes)) = queue.pop_front() else {
            self.pending.remove(link_id);
            return;
        };
        if queue.is_empty() {
            self.pending.remove(link_id);
        }
        // send_large re-queues on TransferInProgress (e.g. a multi-segment
        // transfer that only completed one segment), so nothing is lost.
        self.send_large(*link_id, request_id, bytes).await;
    }
}

/// Where the persistent node identity lives under the data directory.
fn identity_path(data_dir: &Path) -> PathBuf {
    data_dir.join("identities").join("lblogd")
}

/// Build the node's `nomadnetwork.node` destination from its identity.
fn blog_destination(identity: Identity) -> Result<Destination, NodeError> {
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "nomadnetwork",
        &["node"],
    )
    .map_err(|e| NodeError::Destination(e.to_string()))?;
    dest.set_accepts_links(true);
    dest.set_proof_strategy(ProofStrategy::All);
    Ok(dest)
}

/// Resolve the node's destination hash from the persistent identity alone,
/// without connecting to a daemon. Generates and saves the identity on first
/// use, so the hash printed before the first serve run stays valid.
pub fn resolve_destination_hash(data_dir: &Path) -> Result<DestinationHash, NodeError> {
    let identity = load_or_generate_identity(&identity_path(data_dir))?;
    Ok(*blog_destination(identity)?.hash())
}

/// The request paths the node serves for these posts: the index page plus
/// one page per post. Matches the keys `build_pages` renders.
pub fn page_paths(posts: &[Post]) -> Vec<String> {
    std::iter::once(INDEX_PATH.to_string())
        .chain(posts.iter().map(post_page_path))
        .collect()
}

/// The request path a post's page is served under.
fn post_page_path(post: &Post) -> String {
    format!("/page/{}.mu", post.slug)
}

/// Load the persistent identity from `path`, generating and saving a fresh
/// one on first run.
fn load_or_generate_identity(path: &Path) -> Result<Identity, NodeError> {
    let io_err = |source| NodeError::IdentityIo {
        path: path.display().to_string(),
        source,
    };
    if path.exists() {
        let bytes = std::fs::read(path).map_err(io_err)?;
        Identity::from_private_key_bytes(&bytes).map_err(|e| NodeError::Identity {
            path: path.display().to_string(),
            message: format!("bad identity file: {e}"),
        })
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let id = leviculum_std::generate_identity();
        let pk = id.private_key_bytes().map_err(|e| NodeError::Identity {
            path: path.display().to_string(),
            message: format!("key export failed: {e}"),
        })?;
        std::fs::write(path, pk).map_err(io_err)?;
        Ok(id)
    }
}

/// Render every page and encode each as the single msgpack bin value the
/// response APIs expect (the `[request_id, response]` wrapper is added by
/// `send_response`/`send_response_resource` internally).
fn build_pages(posts: &[Post]) -> Result<HashMap<String, Vec<u8>>, NodeError> {
    let mut pages = HashMap::new();
    pages.insert(
        INDEX_PATH.to_string(),
        msgpack_bin(render_index_micron(posts).as_bytes())?,
    );
    for post in posts {
        pages.insert(
            post_page_path(post),
            msgpack_bin(render_post_micron(post).as_bytes())?,
        );
    }
    Ok(pages)
}

/// Encode bytes as one msgpack bin value, the page response payload contract
/// NomadNet clients decode.
fn msgpack_bin(data: &[u8]) -> Result<Vec<u8>, NodeError> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &rmpv::Value::Binary(data.to_vec()))
        .map_err(|e| NodeError::Encode(e.to_string()))?;
    Ok(buf)
}
