//! The propagation node must survive being started before the shared-instance
//! daemon is listening (Codeberg #311's shape, on lnpnd).
//!
//! `lnpnd.service` and the daemon come up in the same transaction, and the
//! daemon's IPC socket is not bound yet when lnpnd dials it: `lnsd` is
//! `Type=simple`, so systemd calls it started the moment it is exec'd,
//! seconds before it listens, and on a package install the two
//! `systemctl start` calls are not ordered against each other at all. The
//! gap reported on lblogd, the same shape on the same host, was three
//! seconds.
//!
//! Every test here runs a real daemon over a real abstract Unix socket;
//! nothing is mocked, because the condition under test is exactly "is that
//! socket accepting yet".

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder, StdNodeCore};
use leviculum_std::ReticulumNode;

/// Stands in for the engine: what is under test is the daemon's connection
/// to the shared instance, not what its processor does with events.
struct Silent;

impl CoreProcessor for Silent {
    fn on_event(&mut self, _core: &mut StdNodeCore, _event: &NodeEvent) -> TickOutput {
        TickOutput::empty()
    }
}

/// A daemon built but not started: its shared-instance socket is not bound
/// until `start()` is awaited, which is the window the bug lives in.
fn build_daemon(instance_name: &str, storage: &std::path::Path) -> ReticulumNode {
    // `:0`: the kernel picks the port, nothing dials this server; the test
    // is wired entirely over the shared instance.
    let daemon_tcp: SocketAddr = "127.0.0.1:0".parse().expect("loopback address");
    ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.to_string())
        .add_tcp_server(daemon_tcp)
        .storage_path(storage.to_path_buf())
        .build_sync()
        .expect("build daemon")
}

/// The daemon node exactly as `lnpnd`'s `main` builds it.
async fn build_lnpnd(instance_name: &str, storage: &std::path::Path) -> ReticulumNode {
    lnpnd::node_builder(Silent, storage.to_path_buf())
        .connect_to_shared_instance(instance_name)
        .build()
        .await
        .expect("build the lnpnd node")
}

/// The bug: a daemon that is merely a couple of seconds late must not cost
/// the propagation node its start.
#[tokio::test]
async fn start_survives_a_daemon_that_is_not_up_yet() {
    let instance_name = format!("lnpnd-311-late-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = build_daemon(&instance_name, daemon_storage.path());

    // The daemon comes up two seconds after lnpnd dials it — a little under
    // the three seconds the issue reported from a PineNote.
    let daemon_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        daemon.start().await.expect("start daemon");
        // Hold the daemon (and its socket) for the rest of the test.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let storage = tempfile::tempdir().expect("lnpnd storage");
    let mut node = build_lnpnd(&instance_name, storage.path()).await;

    let started = Instant::now();
    lnpnd::start_waiting(&mut node, Duration::from_secs(20))
        .await
        .expect("the node must wait for the daemon instead of exiting");
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the node cannot have connected before the daemon was listening"
    );

    daemon_task.abort();
}

/// The wait is bounded, so an `instance_name` no daemon will ever serve
/// still fails the start instead of leaving the unit `active` and mute —
/// which for a propagation node means an announced mailbox that answers
/// nobody.
#[tokio::test]
async fn start_waiting_still_fails_when_no_daemon_ever_appears() {
    let instance_name = format!("lnpnd-311-absent-{}", std::process::id());
    let storage = tempfile::tempdir().expect("lnpnd storage");
    let mut node = build_lnpnd(&instance_name, storage.path()).await;

    // Two polls' worth: long enough that a wait which did not actually
    // wait is visible in the clock, short enough not to cost the suite a
    // minute. The production bound is `lnpnd::DAEMON_WAIT`.
    let started = Instant::now();
    let result = lnpnd::start_waiting(&mut node, Duration::from_millis(2500)).await;

    let error = result.expect_err("no daemon means no node");
    assert!(
        error.to_string().contains("is lnsd or rnsd running?"),
        "the failure must still name the absent daemon, got: {error}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "it must have actually waited before giving up (took {:?})",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the wait is bounded, not endless (took {:?})",
        started.elapsed()
    );
}

/// The plain `start` keeps its fail-fast contract. `lnpnd`'s query verbs
/// (`--status`, `--peers`) run as clients against a running node and want
/// an absent daemon reported at once, not waited on; so does anything else
/// that drives a node built by `node_builder`.
#[tokio::test]
async fn plain_start_still_fails_fast() {
    let instance_name = format!("lnpnd-311-fast-{}", std::process::id());
    let storage = tempfile::tempdir().expect("lnpnd storage");
    let mut node = build_lnpnd(&instance_name, storage.path()).await;

    let started = Instant::now();
    let result = node.start().await;

    assert!(result.is_err(), "no daemon means no node");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "start must not have waited (took {:?})",
        started.elapsed()
    );
}
