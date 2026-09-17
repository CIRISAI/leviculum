//! mvr: a shared-instance client must survive a restart of the daemon.
//!
//! ## The failure
//!
//! Production host, 2026-09-17: after `systemctl restart lnsd`, `lblogd`
//! stayed `active` and logged nothing at all, but was off the mesh —
//! `lnomad` answered "no path to destination" until `lblogd` itself was
//! restarted. The mechanism was written down in our own code: every
//! shared-instance client goes through `spawn_local_client`, whose doc
//! comment said "No reconnection". The client's I/O task returned on the
//! EOF the departing daemon left behind, its incoming channel closed, the
//! driver detached the interface, and the client was left with no
//! interfaces at all — silently, because nothing on that path logs.
//!
//! Python-RNS clients reconnect (`LocalClientInterface.reconnect`,
//! LocalInterface.py:160-192) and re-announce their destinations once back
//! (`Transport.shared_connection_reappeared`, Transport.py:3158-3162).
//!
//! ## Topology
//!
//! ```text
//!   client node (connect_to_shared_instance)
//!        │  abstract unix socket \0rns/<name>
//!        v
//!   daemon node (share_instance)   <- stopped, then started again
//! ```
//!
//! No hardware, no Docker, no Python; two in-process nodes over the real
//! IPC socket, seconds.
//!
//! **Acceptance**: red before the reconnecting client (the restarted daemon
//! never hears from the client again), green once the client reconnects and
//! re-announces on the recovered interface.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationHash, DestinationType, Direction};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use rand_core::OsRng;

/// Unique per test in this binary, so two mvrs never share a socket name.
static INSTANCE_SEQ: AtomicUsize = AtomicUsize::new(0);

fn instance_name() -> String {
    format!(
        "mvr_reconnect_{}_{}",
        std::process::id(),
        INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// A fresh SINGLE destination and its hash.
fn destination(aspect: &str) -> (Destination, DestinationHash) {
    let dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvrreconnect",
        &[aspect],
    )
    .expect("destination");
    let hash = *dest.hash();
    (dest, hash)
}

/// Poll `cond` every 50 ms until it holds or the deadline passes.
async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

/// Whether anything still listens on the instance's abstract socket.
fn socket_is_bound(name: &str) -> bool {
    use std::os::linux::net::SocketAddrExt;
    let Ok(addr) =
        std::os::unix::net::SocketAddr::from_abstract_name(format!("rns/{name}").as_bytes())
    else {
        return false;
    };
    std::os::unix::net::UnixStream::connect_addr(&addr).is_ok()
}

/// Build and start a shared-instance daemon on `name`.
async fn start_daemon(name: &str, storage: &std::path::Path) -> ReticulumNode {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(name.to_string())
        .storage_path(storage.to_path_buf())
        .build()
        .await
        .expect("build daemon");
    node.start().await.expect("start daemon");
    node
}

/// THE pin: the daemon goes away and comes back, and the client is on the
/// mesh again without anyone touching it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_reattaches_after_the_daemon_restarts() {
    let name = instance_name();

    let daemon_storage = tempfile::tempdir().expect("tempdir daemon");
    let mut daemon = start_daemon(&name, daemon_storage.path()).await;

    let client_storage = tempfile::tempdir().expect("tempdir client");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(name.clone())
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("build client");
    client.start().await.expect("start client");

    // Positive control: traffic flows over the IPC before anything is torn
    // down. Without this the test could go green on a client that never
    // worked at all.
    let (dest, dest_hash) = destination("before");
    client.register_destination(dest);
    client
        .announce_destination(&dest_hash, Some(b"before"))
        .await
        .expect("announce before");
    assert!(
        wait_until(Duration::from_secs(10), || daemon.has_path(&dest_hash)).await,
        "baseline: the daemon must learn the client's destination over the IPC"
    );

    // The daemon's own destination reaches the client, so the client holds a
    // path whose next hop is the IPC uplink. That path is what a daemon
    // restart invalidates: the daemon that comes back has an empty path
    // table and cannot place a packet sent along it.
    let (daemon_dest, stale_hash) = destination("stale");
    daemon.register_destination(daemon_dest);
    daemon
        .announce_destination(&stale_hash, Some(b"stale"))
        .await
        .expect("announce stale");
    assert!(
        wait_until(Duration::from_secs(10), || client.has_path(&stale_hash)).await,
        "baseline: the client must learn the daemon's destination over the IPC"
    );

    // The restart. `stop()` shuts the node's runtime down in the background,
    // which drops the accept loop and releases the abstract socket; wait for
    // that release rather than assume it, so the second bind cannot race it.
    daemon.stop().await.expect("stop daemon");
    drop(daemon);
    assert!(
        wait_until(Duration::from_secs(10), || !socket_is_bound(&name)).await,
        "the stopped daemon must release the shared-instance socket"
    );

    // Losing the daemon drops the routing state cached against the uplink:
    // the client must stop claiming a path it can no longer use. Python
    // clears the same tables on the same occasion
    // (`Transport.shared_connection_disappeared`).
    assert!(
        wait_until(Duration::from_secs(10), || !client.has_path(&stale_hash)).await,
        "the client must drop the paths it learned through the departed daemon"
    );

    let daemon2_storage = tempfile::tempdir().expect("tempdir daemon2");
    let daemon2 = start_daemon(&name, daemon2_storage.path()).await;

    // Nothing touches the client from here on. A restarted daemon starts with
    // an empty path table, so the only way it learns the client's destination
    // is the client reconnecting and re-announcing on the recovered
    // interface.
    assert!(
        !daemon2.has_path(&dest_hash),
        "a freshly started daemon cannot already know the client's destination"
    );
    assert!(
        wait_until(Duration::from_secs(20), || daemon2.has_path(&dest_hash)).await,
        "the client must reconnect to the restarted daemon and re-announce on it"
    );

    // Traffic flows in the other direction too: the daemon's own destination
    // reaches the client over the recovered IPC.
    let (daemon2_dest, daemon_hash) = destination("daemonside");
    daemon2.register_destination(daemon2_dest);
    daemon2
        .announce_destination(&daemon_hash, Some(b"daemonside"))
        .await
        .expect("announce daemon side");
    assert!(
        wait_until(Duration::from_secs(10), || client.has_path(&daemon_hash)).await,
        "the client must receive traffic from the restarted daemon"
    );

    client.stop().await.expect("stop client");
}
