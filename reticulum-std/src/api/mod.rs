//! Curated consumer facade for Leviculum.
//!
//! This module is the stable, minimal public API meant for application
//! developers and for the C FFI in `reticulum-ffi`. It selects the small set
//! of app relevant entry points out of the larger `driver` surface and gives
//! them types that hide core internals, so consumers never depend on
//! `reticulum_core` directly.
//!
//! It is additive and adds no behaviour: every method is a thin re-projection
//! of the existing engine in [`crate::driver`]. The design of record is
//! `docs/leviculum-api-design.md`.
//!
//! v1 scope grows in phases: this module currently covers instance lifecycle,
//! identity, and version. Destinations, announce, paths, links, datagrams,
//! requests, resources, and the event stream land in later phases.

use std::path::PathBuf;

use crate::driver::{ReticulumNode, ReticulumNodeBuilder};

pub use crate::error::{Error as ApiError, Result};
pub use crate::{Destination, DestinationHash, DestinationType, Direction, LinkHandle, LinkId};
pub use reticulum_core::Identity;

/// Generate a new random identity using the system RNG.
///
/// Convenience re-export of [`crate::generate_identity`] under the facade.
pub fn generate_identity() -> Identity {
    crate::generate_identity()
}

/// Semantic version of the facade, as `(major, minor, patch)`.
///
/// Sourced from the crate version at compile time.
pub fn version() -> (u16, u16, u16) {
    (
        env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0),
        env!("CARGO_PKG_VERSION_MINOR").parse().unwrap_or(0),
        env!("CARGO_PKG_VERSION_PATCH").parse().unwrap_or(0),
    )
}

/// Version string of the facade, for example `"0.6.3"`.
pub fn version_string() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Builder for a [`Node`].
///
/// Thin wrapper over [`ReticulumNodeBuilder`] that exposes only the
/// app relevant configuration. Consumes itself on each setter, builder style.
#[derive(Default)]
pub struct NodeBuilder {
    inner: ReticulumNodeBuilder,
}

impl NodeBuilder {
    /// Create a new builder with defaults.
    pub fn new() -> Self {
        Self {
            inner: ReticulumNodeBuilder::new(),
        }
    }

    /// Use an explicit identity instead of generating one.
    pub fn identity(mut self, identity: Identity) -> Self {
        self.inner = self.inner.identity(identity);
        self
    }

    /// Set the storage directory for identity, known destinations, and ratchets.
    pub fn storage_path(mut self, path: PathBuf) -> Self {
        self.inner = self.inner.storage_path(path);
        self
    }

    /// Add a TCP client interface to a remote Reticulum node.
    pub fn add_tcp_client(mut self, addr: std::net::SocketAddr) -> Self {
        self.inner = self.inner.add_tcp_client(addr);
        self
    }

    /// Add a TCP server interface listening for inbound connections.
    pub fn add_tcp_server(mut self, addr: std::net::SocketAddr) -> Self {
        self.inner = self.inner.add_tcp_server(addr);
        self
    }

    /// Add a UDP interface (one datagram is one packet).
    pub fn add_udp(
        mut self,
        listen_addr: std::net::SocketAddr,
        forward_addr: std::net::SocketAddr,
    ) -> Self {
        self.inner = self.inner.add_udp_interface(listen_addr, forward_addr);
        self
    }

    /// Add an AutoInterface (IPv6 multicast LAN discovery) with defaults.
    pub fn add_auto_interface(mut self) -> Self {
        self.inner = self.inner.add_auto_interface();
        self
    }

    /// Enable or disable transport (relay and routing) mode.
    pub fn enable_transport(mut self, enabled: bool) -> Self {
        self.inner = self.inner.enable_transport(enabled);
        self
    }

    /// Build the node without entering an async context.
    ///
    /// The node is created and its identity loaded or generated, but the event
    /// loop is not started. Call [`Node::start`] to bring it online.
    pub fn build(self) -> Result<Node> {
        Ok(Node {
            inner: self.inner.build_sync()?,
        })
    }
}

/// A running or stopped Reticulum node.
///
/// Thin wrapper over [`ReticulumNode`] exposing the app relevant lifecycle.
/// The tokio runtime and event loop are owned internally.
pub struct Node {
    inner: ReticulumNode,
}

impl Node {
    /// Start the node: spawn the event loop and bring up interfaces.
    pub async fn start(&mut self) -> Result<()> {
        self.inner.start().await
    }

    /// Stop the node, persist state, and tear down the event loop.
    pub async fn stop(&mut self) -> Result<()> {
        self.inner.stop().await
    }

    /// Whether the event loop is running.
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// The node's own identity hash (16 bytes).
    pub fn identity_hash(&self) -> [u8; 16] {
        self.inner.identity_hash()
    }

    /// Take the engine event receiver, once.
    ///
    /// The C FFI bridge owns this to drain events onto its pollable fd. Returns
    /// `None` if already taken or if the node was built without events.
    pub fn take_event_receiver(&mut self) -> Option<crate::driver::EventReceiver> {
        self.inner.take_event_receiver()
    }

    /// Register a local destination so the node can announce it and accept
    /// links or packets for it.
    pub fn register_destination(&self, destination: Destination) {
        self.inner.register_destination(destination);
    }

    /// Announce a registered destination on all interfaces.
    ///
    /// `app_data` is optional application payload carried in the announce.
    pub async fn announce(
        &self,
        dest_hash: &DestinationHash,
        app_data: Option<&[u8]>,
    ) -> Result<()> {
        self.inner.announce_destination(dest_hash, app_data).await
    }

    /// Whether a path to the destination is known.
    pub fn has_path(&self, dest_hash: &DestinationHash) -> bool {
        self.inner.has_path(dest_hash)
    }

    /// Hop count to the destination, if a path is known.
    pub fn hops_to(&self, dest_hash: &DestinationHash) -> Option<u8> {
        self.inner.hops_to(dest_hash)
    }

    /// The cached identity for a destination, learned from an announce.
    pub fn get_identity(&self, dest_hash: &DestinationHash) -> Option<Identity> {
        self.inner.get_identity(dest_hash)
    }

    /// Request a path to a destination. The result arrives as a path-found event.
    pub async fn request_path(&self, dest_hash: &DestinationHash) -> Result<()> {
        self.inner.request_path(dest_hash).await
    }

    /// Open a link to a destination, given its Ed25519 signing key.
    pub async fn connect_with_key(
        &self,
        dest_hash: &DestinationHash,
        signing_key: &[u8; 32],
    ) -> Result<LinkHandle> {
        self.inner.connect(dest_hash, signing_key).await
    }

    /// Accept an incoming link request from a link-request event.
    pub async fn accept_link(&self, link_id: &LinkId) -> Result<LinkHandle> {
        self.inner.accept_link(link_id).await
    }

    /// Access the underlying engine node.
    ///
    /// Escape hatch while the facade is incomplete: later phases re-project the
    /// remaining methods (destinations, links, events) so consumers will not
    /// need this. Not part of the stable surface.
    pub fn engine(&self) -> &ReticulumNode {
        &self.inner
    }

    /// Mutable access to the underlying engine node. See [`Node::engine`].
    pub fn engine_mut(&mut self) -> &mut ReticulumNode {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_crate() {
        let (major, minor, patch) = version();
        let s = format!("{major}.{minor}.{patch}");
        assert_eq!(s, version_string());
    }

    #[test]
    fn generated_identity_has_private_keys() {
        let id = generate_identity();
        assert!(id.has_private_keys());
        assert_eq!(id.hash().len(), 16);
    }

    #[tokio::test]
    async fn register_and_announce_single_destination() {
        let id = generate_identity();
        let mut node = NodeBuilder::new()
            .identity(id.clone())
            .storage_path(std::env::temp_dir().join("leviculum-api-test-announce"))
            .enable_transport(false)
            .build()
            .expect("build node");
        node.start().await.expect("start node");

        let dest = Destination::new(
            Some(id),
            Direction::In,
            DestinationType::Single,
            "leviculum-test",
            &["api"],
        )
        .expect("build destination");
        let dh = *dest.hash();
        node.register_destination(dest);
        // With no interfaces the announce reaches nobody, but the action path
        // must succeed.
        node.announce(&dh, Some(b"hi")).await.expect("announce");

        node.stop().await.expect("stop node");
    }

    #[tokio::test]
    async fn node_lifecycle_without_interfaces() {
        let mut node = NodeBuilder::new()
            .storage_path(std::env::temp_dir().join("leviculum-api-test-lifecycle"))
            .enable_transport(false)
            .build()
            .expect("build node");
        assert!(!node.is_running());
        node.start().await.expect("start node");
        assert!(node.is_running());
        // Identity hash is stable and 16 bytes.
        assert_eq!(node.identity_hash().len(), 16);
        node.stop().await.expect("stop node");
        assert!(!node.is_running());

        // Restart: stop then start brings the node back up (the engine
        // rebuilds its runtime on start).
        node.start().await.expect("restart node");
        assert!(node.is_running());
        node.stop().await.expect("stop node again");
        assert!(!node.is_running());
    }
}
