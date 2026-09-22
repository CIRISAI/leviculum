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
use std::time::Duration;

use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder};
use leviculum_std::{Error as StdError, ReticulumNode};

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

/// How long [`start_waiting`] keeps re-dialling the shared-instance daemon
/// before it gives up and fails the start.
///
/// Bounded rather than endless, because "the socket is not bound yet" and
/// "no daemon will ever serve this `instance_name`" look identical from
/// here, and a node that waits for ever on the second reports itself
/// healthy while carrying nobody's messages — the worst failure mode a
/// propagation node has, since its clients see an announced node that
/// never answers.
///
/// Sixty seconds, the same bound `lblogd` took for the same race, and
/// borrowed rather than derived a second time: the measurement behind it
/// is the one from Codeberg #311, a three-second gap between the failed
/// start and `lnsd`'s "Reticulum daemon running" on an aarch64 PineNote.
/// Sixty is far above that and far below the point where an operator
/// reading `systemctl status` would call the unit hung. The unit's
/// `Restart=` stays the outer loop, so a daemon that takes longer than
/// this still costs a restart rather than the service.
pub const DAEMON_WAIT: Duration = Duration::from_secs(60);

/// How often the daemon is re-dialled while waiting.
///
/// The dial itself is one failed `connect` syscall, but an attempt is a
/// whole [`ReticulumNode::start`], and the driver logs one INFO line
/// ("Shared instance client mode — skipping config interfaces") per
/// attempt from below this crate. At `lblogd`'s 250 ms that is 240 lines
/// per wait — measured, by running the wait out against an instance nobody
/// serves — and a propagation node whose daemon is gone for good repeats
/// that every `RestartSec`, some 3.4 lines a second into a board's journal
/// for as long as nobody notices. One second keeps the boot race's extra
/// latency under a second, which is nothing against the `RestartSec=10`
/// this replaces, and keeps the journal readable.
const DAEMON_POLL: Duration = Duration::from_secs(1);

/// Whether a failed start failed *only* because no daemon is listening yet.
///
/// The shared-instance socket is the only socket a node built by
/// [`node_builder`] opens — `enable_transport(false)`, and a client-mode
/// node skips every configured interface — so a refused or absent
/// connection during `start` can be nothing else. `leviculum-std`
/// preserves the error kind through the message it wraps the failure in:
/// `ConnectionRefused` for Linux's abstract socket, `NotFound` for the
/// filesystem-socket path the other platforms use.
///
/// Everything else is permanent and is returned at once rather than hidden
/// behind a minute of waiting: an identity file that cannot be read, a
/// storage path that cannot be created, a config that names both
/// `share_instance` and `connect_to_shared_instance`.
fn daemon_absent(error: &StdError) -> bool {
    let StdError::Io(io) = error else {
        return false;
    };
    matches!(
        io.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    )
}

/// [`ReticulumNode::start`], but tolerating a daemon that is not listening
/// yet: retry for up to `wait` before failing.
///
/// This is what a *daemon* wants, and the plain `start` is what a one-shot
/// command wants. At boot lnpnd and the shared instance come up in the same
/// transaction, and ordering alone does not separate them: `lnsd` is
/// `Type=simple`, so systemd calls it started the moment it is exec'd,
/// seconds before it binds its IPC socket — and on a package install the
/// two `systemctl start` calls are not ordered against each other at all.
/// Exiting there writes a failed unit into every boot's journal and leaves
/// `NRestarts` permanently non-zero, which costs that counter its value for
/// spotting a real crash.
///
/// The wait is on the socket, not on a unit, so it holds for the Python
/// `rnsd` exactly as it does for `lnsd`. Once connected, holding the
/// connection across a daemon restart is the local client's own job.
///
/// Retrying `start` in place, rather than rebuilding the node the way
/// `lblogd` does, is what this node's shape allows and wants. `build` does
/// not dial anything — it only records the instance name — and the dial
/// that fails happens in `start`'s interface initialisation, before the
/// node takes its core processor or spawns its event loop, so a node whose
/// `start` failed this way is untouched and can simply be started again.
/// Rebuilding instead would mean re-creating the engine, and the engine
/// owns the opened message store: re-opening that scans the whole store
/// directory, which on a node with a full mailbox would be a full scan four
/// times a second for as long as the daemon is away.
pub async fn start_waiting(node: &mut ReticulumNode, wait: Duration) -> Result<(), StdError> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut said_so = false;
    loop {
        let error = match node.start().await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if !daemon_absent(&error) || tokio::time::Instant::now() + DAEMON_POLL > deadline {
            return Err(error);
        }
        // Once, not per attempt: the point is that the journal says why the
        // node is not serving yet, not that it says so four times a second.
        if !said_so {
            tracing::warn!("lnpnd: {error} — waiting up to {} s for it", wait.as_secs());
            said_so = true;
        }
        tokio::time::sleep(DAEMON_POLL).await;
    }
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
