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
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use leviculum_core::resource::ResourceError;
use leviculum_core::{LinkId, RequestError, RequestPolicy};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::{
    Destination, DestinationHash, DestinationType, Direction, Error as StdError, EventReceiver,
    Identity, NodeEvent, ProofStrategy, ReticulumNode,
};

use crate::content::{Snapshot, SnapshotRx};

/// Truncated request id length (matches the driver's request/response API).
const REQUEST_ID_LEN: usize = 16;

/// A queued large response: the request id it answers and the encoded page.
type PendingResponse = ([u8; REQUEST_ID_LEN], Vec<u8>);

/// Errors from building or running the blog node.
#[derive(Debug, Error)]
pub enum NodeError {
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
    /// The content currently served, replaced wholesale on reload.
    snapshot: Arc<Snapshot>,
    /// Reload channel; every change swaps [`snapshot`](Self::snapshot) and
    /// reconciles the registered request handlers with it.
    content: SnapshotRx,
    /// Large responses waiting for the link's outgoing resource slot: a link
    /// carries only one outgoing resource at a time, so concurrent large-page
    /// requests on the same link queue here until the slot frees up.
    pending: HashMap<LinkId, VecDeque<PendingResponse>>,
}

impl BlogNode {
    /// Connect to the shared instance, load the identity, and register the
    /// destination and one request handler per page in the current snapshot.
    pub async fn start(config: BlogNodeConfig, content: SnapshotRx) -> Result<Self, NodeError> {
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

        let snapshot = content.borrow().clone();
        // One handler per exact path: the wire carries only the truncated
        // path hash, so prefix or wildcard registration is impossible by
        // design. Unregistered paths are silently dropped by the stack.
        for path in snapshot.pages.keys() {
            node.register_request_handler(dest_hash, path, RequestPolicy::AllowAll);
        }

        Ok(BlogNode {
            node,
            events,
            dest_hash,
            display_name: config.display_name,
            announce_interval: config.announce_interval,
            snapshot,
            content,
            pending: HashMap::new(),
        })
    }

    /// The node's destination hash (what a browser dials).
    pub fn destination_hash(&self) -> DestinationHash {
        self.dest_hash
    }

    /// The request paths this node serves, sorted.
    pub fn served_paths(&self) -> Vec<String> {
        self.snapshot.served_paths()
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
                changed = self.content.changed() => {
                    // Err means the Reloader was dropped: no further reloads,
                    // but the current snapshot stays perfectly serveable.
                    if changed.is_ok() {
                        let snapshot = self.content.borrow_and_update().clone();
                        self.apply_snapshot(snapshot);
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

    /// Swap in new content and reconcile the registered handlers with it.
    ///
    /// Paths that vanished are deregistered, so a deleted post stops being
    /// served rather than lingering until restart; new paths are registered.
    /// Registration is an idempotent map insert, so unchanged paths are left
    /// alone rather than churned.
    ///
    /// In-flight transfers are unaffected: `respond` copies the page bytes out
    /// of the snapshot before sending, and queued responses in `pending` hold
    /// their own copies, so replacing the snapshot cannot tear a transfer that
    /// is already under way.
    fn apply_snapshot(&mut self, snapshot: Arc<Snapshot>) {
        let (gone, added) = reconcile_paths(&self.snapshot, &snapshot);
        for path in &gone {
            self.node.deregister_request_handler(self.dest_hash, path);
        }
        for path in &added {
            self.node
                .register_request_handler(self.dest_hash, path, RequestPolicy::AllowAll);
        }
        eprintln!(
            "lblogd: reloaded {} posts, serving {} pages",
            snapshot.posts.len(),
            snapshot.pages.len()
        );
        self.snapshot = snapshot;
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
                let Some(bytes) = self.snapshot.pages.get(&path).cloned() else {
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

/// Which request handlers a snapshot swap has to add and remove: paths in
/// `old` that `new` no longer has, and paths `new` has that `old` did not.
/// Both sorted, so a caller's log and a test read deterministically.
///
/// Extracted from [`BlogNode::apply_snapshot`] because the deregistration is
/// invisible from outside the process: with the handler leaked, a request for
/// a deleted page still reaches the node and is dropped by the page lookup in
/// `respond`, so a client sees the same timeout either way. The end-to-end
/// test in `tests/node_integ.rs` therefore passes with the deregistration loop
/// deleted (verified 2026-07-30). What leaking costs is a handler map that
/// grows with every reload and a node that accepts requests for pages it no
/// longer serves — so the decision is what gets pinned.
fn reconcile_paths(old: &Snapshot, new: &Snapshot) -> (Vec<String>, Vec<String>) {
    let mut gone: Vec<String> = old
        .pages
        .keys()
        .filter(|p| !new.pages.contains_key(*p))
        .cloned()
        .collect();
    let mut added: Vec<String> = new
        .pages
        .keys()
        .filter(|p| !old.pages.contains_key(*p))
        .cloned()
        .collect();
    gone.sort();
    added.sort();
    (gone, added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::BlogMeta;

    /// A snapshot that has exactly the given page paths; nothing else in it
    /// matters to `reconcile_paths`.
    fn snapshot_with(paths: &[&str]) -> Snapshot {
        Snapshot {
            meta: BlogMeta::default(),
            posts: Vec::new(),
            pages: paths
                .iter()
                .map(|p| ((*p).to_string(), Vec::new()))
                .collect(),
            css: String::new(),
            about: None,
        }
    }

    #[test]
    fn a_vanished_page_is_deregistered_and_a_new_one_registered() {
        let old = snapshot_with(&["/page/index.mu", "/page/second.mu"]);
        let new = snapshot_with(&["/page/index.mu", "/page/third.mu"]);
        let (gone, added) = reconcile_paths(&old, &new);
        assert_eq!(
            gone,
            vec!["/page/second.mu"],
            "a page that vanished from the snapshot must have its handler removed"
        );
        assert_eq!(
            added,
            vec!["/page/third.mu"],
            "a page the reload added must get a handler"
        );
    }

    #[test]
    fn an_unchanged_page_is_left_alone() {
        let pages = ["/page/index.mu", "/page/hello.mu"];
        let (gone, added) = reconcile_paths(&snapshot_with(&pages), &snapshot_with(&pages));
        assert!(
            gone.is_empty() && added.is_empty(),
            "an unchanged path must be neither churned nor dropped, got \
             gone={gone:?} added={added:?}"
        );
    }
}
