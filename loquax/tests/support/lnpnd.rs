//! A propagation node on a test's shared instance: lnpnd's engine, no
//! mailbox of its own, announced at once. Shared by `tui_snapshot.rs` and
//! `home_node.rs`.

// Each test crate includes this module and uses its own part of it.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use lnpnd::engine::{Engine, EngineConfig, EngineEvent};

/// The node's hash, and what keeps it alive.
pub struct PropagationNode {
    pub hash: [u8; 16],
    pub node: leviculum_std::ReticulumNode,
    pub events: std::sync::mpsc::Receiver<EngineEvent>,
    pub storage: tempfile::TempDir,
}

impl PropagationNode {
    /// lnpnd on the shared instance `instance`, asking `stamp_cost` of
    /// every upload.
    pub async fn start(instance: &str, stamp_cost: u8) -> Self {
        let (engine, events) = Engine::new(EngineConfig {
            identity: leviculum_std::generate_identity(),
            node_config: leviculum_lxmf::PropagationNodeConfig {
                stamp_cost,
                always_compute_stamp_value: false,
                ..leviculum_lxmf::PropagationNodeConfig::default()
            },
            store: leviculum_lxmf::MemoryPropagationStore::new(64_000),
            announce_interval_secs: 3600,
            announce_delay_secs: 0,
            announce_at_start: false,
            peering: leviculum_lxmf::PeeringConfig::default(),
            peer_store: Box::new(leviculum_lxmf::MemoryPeerStore::default()),
            control_allowed: Vec::new(),
            auth_allowed: None,
            mailbox: None,
            store_limit_bytes: 64_000,
            delivery_limit_kb: 1000,
        });
        let storage = tempfile::tempdir().expect("pn storage");
        let mut node = lnpnd::node_builder(engine, storage.path().to_path_buf())
            .connect_to_shared_instance(instance)
            .build()
            .await
            .expect("build the propagation node");
        node.start().await.expect("attach the propagation node");
        let deadline = Instant::now() + Duration::from_secs(10);
        let hash = loop {
            match events.try_recv() {
                Ok(EngineEvent::Ready { destination_hash }) => break destination_hash,
                Ok(_) => {}
                Err(_) => {
                    assert!(
                        Instant::now() < deadline,
                        "the propagation node never got ready"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        Self {
            hash,
            node,
            events,
            storage,
        }
    }

    pub fn hex(&self) -> String {
        hex::encode(self.hash)
    }
}
