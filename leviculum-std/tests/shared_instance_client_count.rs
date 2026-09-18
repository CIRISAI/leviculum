//! What the shared instance answers when asked how many programs it serves.
//!
//! Production host, 2026-09-17: `lnstatus` printed `Serving   : 0 programs`
//! while `lblogd` was attached to the shared instance and carrying mesh
//! traffic through it. The daemon's number was not the fault — this test
//! pins it — the printing side was (`lnstatus_render::StatusOptions::
//! observer_clients`).
//!
//! The number the daemon must produce is `LocalServerInterface.clients`:
//! zero at startup (LocalInterface.py:384), one up per accepted IPC client
//! (LocalInterface.py:463), one down when a client tears down
//! (LocalInterface.py:355). It counts attached programs and nothing else —
//! the shared-instance server is not one of its own clients, and a caller of
//! the `interface_stats` RPC only enters the count if it is separately
//! attached, the way `rnstatus` is (Reticulum.py:421-436).
//!
//! In-process daemon plus one in-process client over the real abstract unix
//! socket; no Python, no Docker, about a second.

use std::time::{Duration, Instant};

use leviculum_core::Identity;
use leviculum_std::driver::ReticulumNodeBuilder;
use rand_core::OsRng;
use serde_json::Value;

/// RPC authkey the daemon derives from its transport identity: SHA256 over
/// the 64 private key bytes.
fn authkey_of(identity: &Identity) -> [u8; 32] {
    use sha2::Digest;
    let mut key = [0u8; 32];
    key.copy_from_slice(&sha2::Sha256::digest(
        identity.private_key_bytes().expect("private key bytes"),
    ));
    key
}

/// `clients` on the `Shared Instance[...]` row of a live `interface_stats`.
async fn served_clients(instance: &str, authkey: &[u8; 32]) -> i64 {
    let stats = leviculum_std::rpc_query(instance, authkey, "interface_stats")
        .await
        .expect("interface_stats RPC");
    let ifaces = stats["interfaces"]
        .as_array()
        .cloned()
        .expect("interfaces list");
    let row = ifaces
        .iter()
        .find(|i| {
            i["name"]
                .as_str()
                .is_some_and(|n| n.starts_with("Shared Instance["))
        })
        .unwrap_or_else(|| panic!("no Shared Instance row in {ifaces:?}"));
    assert!(
        matches!(row["clients"], Value::Number(_)),
        "the shared instance must report a client count, got {:?}",
        row["clients"]
    );
    row["clients"].as_i64().expect("clients is an integer")
}

/// Poll `served_clients` until it reads `want`, or fail with what it read.
async fn wait_for_clients(instance: &str, authkey: &[u8; 32], want: i64, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = -1;
    while Instant::now() < deadline {
        last = served_clients(instance, authkey).await;
        if last == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what}: shared instance reported {last} clients, expected {want}");
}

#[tokio::test]
async fn shared_instance_counts_attached_programs() {
    let identity = Identity::generate(&mut OsRng);
    let authkey = authkey_of(&identity);
    let instance = format!("clientcount_{}", std::process::id());
    let storage = tempfile::tempdir().expect("daemon storage");

    let mut daemon = ReticulumNodeBuilder::new()
        .identity(identity)
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build daemon");
    daemon.start().await.expect("start daemon");

    // No program attached. The RPC caller asking the question is not one:
    // it reaches the daemon over `\0rns/<instance>/rpc` and never becomes an
    // interface.
    assert_eq!(
        served_clients(&instance, &authkey).await,
        0,
        "an idle shared instance serves nobody"
    );

    // One attached program (an `lblogd` stand-in).
    let client_storage = tempfile::tempdir().expect("client storage");
    let mut client = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&instance)
        .storage_path(client_storage.path().to_path_buf())
        .build()
        .await
        .expect("build client");
    client.start().await.expect("start client");

    wait_for_clients(&instance, &authkey, 1, "one attached program").await;

    // And back down when it leaves (LocalInterface.py:355).
    client.stop().await.expect("stop client");
    wait_for_clients(&instance, &authkey, 0, "after the program detached").await;

    daemon.stop().await.expect("stop daemon");
}
