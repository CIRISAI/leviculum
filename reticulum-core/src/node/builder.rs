//! NodeCore builder pattern implementation
//!
//! The [`NodeCoreBuilder`] provides a fluent API for configuring and creating
//! a [`NodeCore`] instance.

use crate::destination::ProofStrategy;
use crate::identity::Identity;
use crate::resource::RESOURCE_MAX_INCOMING_SIZE;
use crate::transport::TransportConfig;
use rand_core::CryptoRngCore;

use super::NodeCore;

/// Builder for creating [`NodeCore`] instances
///
/// # Example
///
/// ```no_run
/// use reticulum_core::node::NodeCoreBuilder;
/// use reticulum_core::identity::Identity;
/// use reticulum_core::ProofStrategy;
/// use reticulum_core::traits::{Clock, NoStorage};
/// # use core::cell::Cell;
/// # struct MyClock(Cell<u64>);
/// # impl MyClock { fn new(ms: u64) -> Self { Self(Cell::new(ms)) } }
/// # impl Clock for MyClock { fn now_ms(&self) -> u64 { self.0.get() } }
///
/// # fn example() {
/// let my_identity = Identity::generate(&mut rand_core::OsRng);
///
/// let node = NodeCoreBuilder::new()
///     .identity(my_identity)
///     .proof_strategy(ProofStrategy::All)
///     .enable_transport(true)
///     .build(rand_core::OsRng, MyClock::new(0), NoStorage);
/// # }
/// ```
pub struct NodeCoreBuilder {
    identity: Option<Identity>,
    proof_strategy: ProofStrategy,
    transport_config: TransportConfig,
    respond_to_probes: bool,
    max_incoming_resource_size: usize,
}

impl Default for NodeCoreBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeCoreBuilder {
    /// Create a new builder with default settings
    pub fn new() -> Self {
        Self {
            identity: None,
            proof_strategy: ProofStrategy::None,
            transport_config: TransportConfig::default(),
            respond_to_probes: false,
            max_incoming_resource_size: RESOURCE_MAX_INCOMING_SIZE,
        }
    }

    /// Set the node's identity
    ///
    /// If not set, a new identity will be generated during `build()`.
    pub fn identity(mut self, identity: Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Set the default proof strategy for destinations
    pub fn proof_strategy(mut self, strategy: ProofStrategy) -> Self {
        self.proof_strategy = strategy;
        self
    }

    /// Enable or disable transport mode (routing for other nodes)
    pub fn enable_transport(mut self, enable: bool) -> Self {
        self.transport_config.enable_transport = enable;
        self
    }

    /// Set the maximum number of hops for path finding
    pub fn max_hops(mut self, hops: u8) -> Self {
        self.transport_config.max_hops = hops;
        self
    }

    /// Set the path expiry time in seconds
    pub fn path_expiry_secs(mut self, secs: u64) -> Self {
        self.transport_config.path_expiry_secs = secs;
        self
    }

    /// Set the announce rate limit interval in milliseconds
    pub fn announce_rate_limit_ms(mut self, ms: u64) -> Self {
        self.transport_config.announce_rate_limit_ms = ms;
        self
    }

    /// Set the maximum number of queued announces per capped interface.
    /// Default: 16384. On embedded with limited heap, set to e.g. 32.
    pub fn max_queued_announces(mut self, n: usize) -> Self {
        self.transport_config.max_queued_announces = n;
        self
    }

    /// Set the maximum number of random blobs retained per path entry.
    /// Default: 64. On embedded with limited heap, set to e.g. 8.
    pub fn max_random_blobs(mut self, n: usize) -> Self {
        self.transport_config.max_random_blobs = n;
        self
    }

    /// Override the link keepalive interval (seconds) for every link this node
    /// creates. `None` (default) keeps the RTT-derived interval. Useful for
    /// slow links and for shrinking the stale-link timeout in tests.
    pub fn link_keepalive(mut self, secs: Option<u64>) -> Self {
        self.transport_config.link_keepalive_secs = secs;
        self
    }

    /// Enable probe responder (rnstransport.probe destination with PROVE_ALL).
    ///
    /// When enabled, the node creates a probe destination from its transport
    /// identity at build time and announces it periodically. Other nodes can
    /// then use `rnprobe` to measure RTT via delivery proofs.
    pub fn respond_to_probes(mut self, enable: bool) -> Self {
        self.respond_to_probes = enable;
        self
    }

    /// Set the maximum incoming resource size in bytes.
    ///
    /// Resources advertised with `transfer_size` above this limit are
    /// rejected before any allocation. Default: `usize::MAX` (no limit).
    /// On embedded targets with limited heap, set to e.g. `8 * 1024`.
    pub fn max_incoming_resource_size(mut self, size: usize) -> Self {
        self.max_incoming_resource_size = size;
        self
    }

    /// Set the full transport configuration
    pub fn transport_config(mut self, config: TransportConfig) -> Self {
        self.transport_config = config;
        self
    }

    /// Check if transport mode is enabled in this builder
    pub fn is_transport_enabled(&self) -> bool {
        self.transport_config.enable_transport
    }

    /// Access the identity set on this builder (if any)
    pub fn identity_ref(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// Build the NodeCore instance
    ///
    /// If no identity was provided, a new one will be generated using the RNG.
    ///
    /// # Arguments
    /// * `rng` - Random number generator (moved into NodeCore)
    /// * `clock` - Clock instance (moved into NodeCore)
    /// * `storage` - Storage instance (moved into NodeCore)
    pub fn build<R, Clk, S>(self, mut rng: R, clock: Clk, storage: S) -> NodeCore<R, Clk, S>
    where
        R: CryptoRngCore,
        Clk: crate::traits::Clock,
        S: crate::traits::Storage,
    {
        // Get or generate identity
        let identity = match self.identity {
            Some(id) => id,
            None => Identity::generate(&mut rng),
        };

        let mut node = NodeCore::new(
            identity,
            self.transport_config,
            self.proof_strategy,
            self.max_incoming_resource_size,
            rng,
            clock,
            storage,
        );

        if self.respond_to_probes {
            node.enable_probe_responder();
        }

        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{MockClock, TEST_TIME_MS};
    use crate::traits::NoStorage;
    use rand_core::OsRng;

    #[test]
    fn test_builder_default() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        assert_eq!(node.active_link_count(), 0);
        assert_eq!(node.pending_link_count(), 0);
    }

    #[test]
    fn test_builder_with_identity() {
        let identity = Identity::generate(&mut OsRng);
        let id_hash = *identity.hash();
        let clock = MockClock::new(TEST_TIME_MS);

        let node = NodeCoreBuilder::new()
            .identity(identity)
            .build(OsRng, clock, NoStorage);

        assert_eq!(node.identity().hash(), &id_hash);
    }

    #[test]
    fn test_builder_with_proof_strategy() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new()
            .proof_strategy(ProofStrategy::All)
            .build(OsRng, clock, NoStorage);

        assert_eq!(node.default_proof_strategy(), ProofStrategy::All);
    }

    #[test]
    fn test_builder_with_transport_config() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new()
            .enable_transport(true)
            .max_hops(10)
            .path_expiry_secs(7200)
            .build(OsRng, clock, NoStorage);

        assert!(node.transport_config().enable_transport);
        assert_eq!(node.transport_config().max_hops, 10);
        assert_eq!(node.transport_config().path_expiry_secs, 7200);
    }

    #[test]
    fn test_builder_link_keepalive_reaches_config() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new()
            .link_keepalive(Some(7))
            .build(OsRng, clock, NoStorage);
        assert_eq!(node.transport_config().link_keepalive_secs, Some(7));
    }

    #[test]
    fn test_builder_chaining() {
        let identity = Identity::generate(&mut OsRng);
        let clock = MockClock::new(TEST_TIME_MS);

        let node = NodeCoreBuilder::new()
            .identity(identity)
            .proof_strategy(ProofStrategy::App)
            .enable_transport(false)
            .max_hops(5)
            .path_expiry_secs(3600)
            .announce_rate_limit_ms(5000)
            .build(OsRng, clock, NoStorage);

        assert_eq!(node.default_proof_strategy(), ProofStrategy::App);
        assert!(!node.transport_config().enable_transport);
        assert_eq!(node.transport_config().max_hops, 5);
    }

    #[test]
    fn test_respond_to_probes_creates_destination() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new()
            .respond_to_probes(true)
            .build(OsRng, clock, NoStorage);

        let probe_hash = node.probe_dest_hash();
        assert!(
            probe_hash.is_some(),
            "probe destination should be registered"
        );

        // Verify it's a real registered destination
        let hash = probe_hash.unwrap();
        assert!(node.destination(hash).is_some());
    }

    #[test]
    fn test_respond_to_probes_disabled_by_default() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new().build(OsRng, clock, NoStorage);

        assert!(
            node.probe_dest_hash().is_none(),
            "probe should not be enabled by default"
        );
    }

    #[test]
    fn test_probe_destination_has_prove_all_strategy() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new()
            .respond_to_probes(true)
            .build(OsRng, clock, NoStorage);

        let hash = node.probe_dest_hash().unwrap();
        let dest = node.destination(hash).unwrap();
        assert_eq!(dest.proof_strategy(), ProofStrategy::All);
    }

    #[test]
    fn test_probe_schedules_mgmt_announce() {
        let clock = MockClock::new(TEST_TIME_MS);
        let node = NodeCoreBuilder::new()
            .respond_to_probes(true)
            .build(OsRng, clock, NoStorage);

        // Should have a next_deadline for the mgmt announce (15s after startup)
        let deadline = node.next_deadline();
        assert!(
            deadline.is_some(),
            "mgmt announce should schedule a deadline"
        );
        let expected = TEST_TIME_MS + 15_000;
        assert_eq!(deadline.unwrap(), expected);
    }
}
