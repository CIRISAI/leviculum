//! lnpnd — the LXMF propagation-node daemon (Codeberg #384, parts 1–2).
//!
//! The host-side mailbox role: announce `lxmf.propagation`, accept client
//! uploads, answer `/get`, peer with other propagation nodes over `/offer`
//! and sync stored messages both ways, on the file-backed store. The
//! protocol logic is `leviculum_lxmf::propagation_node` and
//! `leviculum_lxmf::peering`; this crate is wiring, a CLI and a run loop.
//!
//! # Why a separate binary and not an `lnmsg` mode
//!
//! The reference ships the role the same way: `lxmd` is its own console
//! daemon next to the client tools, with its own identity and its own
//! storage (`reference/LXMF/LXMF/Utilities/lxmd.py`), and Sideband or
//! `lnmsg`-shaped clients talk *to* it. Folding the role into `lnmsg` would
//! conflate the operator's messaging address with the node's service
//! identity — a propagation node's destination hash is what every client
//! configures, and it must not change because somebody reinstalled their
//! messenger. The daemon attaches to a running `lnsd`/`rnsd` shared
//! instance exactly like `lnmsg` and `lblogd` do; it does not start a stack
//! of its own.

use std::path::PathBuf;

use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder};

pub mod client;
pub mod config;
pub mod engine;
pub mod identity;
pub mod mailbox;
pub(crate) mod peering;
pub(crate) mod validation;

/// The driver configuration the daemon runs on, minus the shared instance
/// the caller names.
///
/// Split out of `main` so a test can hold it to the property that cost the
/// first field run (Codeberg #419): the engine is a `core_processor` and
/// reads its events off the processor tap, which runs ahead of the driver's
/// application event sink — and nothing in this crate ever takes that sink's
/// receiver. Left enabled, it is a queue with no reader: it fills to its
/// capacity within minutes on the public mesh and then drops every control
/// event for the rest of the daemon's life, 466 of them in the first 26
/// minutes, each with a WARN line addressed to nobody.
pub fn node_builder(processor: impl CoreProcessor, storage: PathBuf) -> ReticulumNodeBuilder {
    ReticulumNodeBuilder::new()
        .enable_transport(false)
        .storage_path(storage)
        .core_processor(processor)
        .without_events()
}

#[cfg(test)]
mod node_builder_tests {
    use super::*;
    use leviculum_core::node::NodeEvent;
    use leviculum_core::transport::TickOutput;
    use leviculum_std::driver::StdNodeCore;

    /// Stands in for the engine: the property under test is the daemon's
    /// driver configuration, not what the processor does with the events.
    struct Silent;

    impl CoreProcessor for Silent {
        fn on_event(&mut self, _core: &mut StdNodeCore, _event: &NodeEvent) -> TickOutput {
            TickOutput::empty()
        }
    }

    /// The daemon must not carry an application event channel: its events go
    /// to the processor tap, and a channel nobody takes only fills up and
    /// starts dropping (Codeberg #419).
    #[tokio::test]
    async fn the_daemon_node_has_no_event_channel_nobody_would_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut node = node_builder(Silent, dir.path().to_path_buf())
            .build()
            .await
            .expect("build the daemon node");
        assert!(
            node.take_event_receiver().is_none(),
            "lnpnd queues node events for a reader it never creates"
        );
    }
}
