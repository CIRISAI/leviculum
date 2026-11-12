//! The Reticulum side end to end, over a real shared-instance IPC path
//! with no Python and no hardware (the topology `lblogd`'s page-node
//! test proved):
//!
//! ```text
//! lnomad Session ── Unix socket ── Rust daemon ── Unix socket ── MapNode
//! (the fetch path)                 (share_instance)              (lev.maps)
//! ```
//!
//! The assertions are the production claims: the index fetched over
//! the mesh verifies against the pinned destination — which IS the
//! destination the node answers on — a piece round-trips byte-exactly
//! under the same id HTTP uses, a piece tampered on the server's disk
//! is refused by the client's verification, and an unknown id is a
//! clean timeout.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::generate_identity;
use lnmapd::node::{MapNode, MapNodeConfig};
use lnmapd::store::{add_pack, piece_path, verify_piece, AddOptions, Index, KIND_VECTORS};
use lnomad::fetch::{FetchError, Session};
use lnomad::url::Target;

fn target(dest: [u8; 16], path: &str, fields: Vec<(String, String)>) -> Target {
    Target {
        dest_hash: dest,
        path: path.to_owned(),
        fields,
        is_file: false,
    }
}

#[tokio::test]
async fn the_map_node_serves_the_signed_store_over_the_mesh() {
    // The daemon: a transport node sharing its instance over IPC,
    // standing in for a production lnsd. `:0`: nothing dials the TCP
    // side, everything runs over the shared instance.
    let daemon_tcp: SocketAddr = "127.0.0.1:0".parse().expect("parse addr");
    let instance_name = format!("lnmapd-k4-test-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_tcp_server(daemon_tcp)
        .storage_path(daemon_storage.path().to_path_buf())
        .build_sync()
        .expect("build daemon");
    daemon.start().await.expect("start daemon");
    // Let the abstract Unix socket listener come up before clients connect.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The store, signed by the server identity lnmapcut and lnmapd
    // share; the map node loads the same file.
    let store = tempfile::tempdir().expect("store dir");
    let data = tempfile::tempdir().expect("data dir");
    let identity_path = data.path().join("identities").join("lnmapd");
    let region = common::grunewald_region();
    let identity = {
        std::fs::create_dir_all(identity_path.parent().expect("parent")).expect("mkdir");
        let id = generate_identity();
        let pk = id.private_key_bytes().expect("export");
        leviculum_std::atomic_write_secret(&identity_path, &pk).expect("write identity");
        id
    };
    let index = add_pack(
        store.path(),
        &region,
        &identity,
        &AddOptions {
            id: "grunewald".to_owned(),
            kind: KIND_VECTORS.to_owned(),
            http_url: None,
        },
    )
    .expect("add the pack");
    let pack = index.pack("grunewald").expect("pack").clone();

    let node = MapNode::start(MapNodeConfig {
        instance_name: instance_name.clone(),
        identity_path,
        storage_dir: data.path().join("storage"),
        store_dir: store.path().to_path_buf(),
        display_name: "map server under test".to_owned(),
        announce_interval: Duration::from_secs(3600),
    })
    .await
    .expect("start map node");
    let dest = *node.destination_hash().as_bytes();

    // The single trust anchor: the destination the node answers on is
    // exactly what the index's embedded key recomputes to.
    let pinned = node.destination_hash();
    let node_task = tokio::spawn(node.run());
    // Let the node's IPC link establish and its announce propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The client: lnomad's real request path over its own IPC
    // connection — the same driver a page fetch uses, not a parallel
    // one.
    let session_storage = tempfile::tempdir().expect("session storage");
    let app_dir = tempfile::tempdir().expect("session app dir");
    let mut session = Session::connect_to_with_app_dir(
        &instance_name,
        session_storage.path().to_path_buf(),
        Some(app_dir.path().to_path_buf()),
    )
    .await
    .expect("session connect");

    // The index over the mesh verifies against the pin.
    let bytes = session
        .fetch(&target(dest, "/index", vec![]), Duration::from_secs(20))
        .await
        .expect("fetch the index over the mesh");
    let fetched = Index::verify(&bytes, &pinned, None).expect("the mesh index verifies");
    assert_eq!(fetched, index, "the index is byte-faithful over the mesh");

    // A piece, by the same id the HTTP side serves.
    let piece = &pack.pieces[0];
    let fields = vec![("piece".to_owned(), piece.id())];
    let bytes = session
        .fetch(
            &target(dest, "/piece", fields.clone()),
            Duration::from_secs(60),
        )
        .await
        .expect("fetch the coarse piece over the mesh");
    verify_piece(piece, &bytes).expect("the mesh piece verifies");

    // The server's disk copy is tampered with: the server serves it
    // blindly (its store is its truth), and the CLIENT's verification
    // is what refuses — that is the design's whole claim, any carrier
    // may be wrong and the pin still decides.
    let path = piece_path(store.path(), &piece.id());
    let mut poisoned = std::fs::read(&path).expect("read piece");
    poisoned[0] ^= 1;
    std::fs::write(&path, &poisoned).expect("poison the store");
    let bytes = session
        .fetch(&target(dest, "/piece", fields), Duration::from_secs(60))
        .await
        .expect("the tampered piece still arrives");
    assert!(
        verify_piece(piece, &bytes).is_err(),
        "the client must refuse a piece that does not hash to its name"
    );

    // An unknown id is dropped silently by the node: a clean timeout,
    // no crash — the protocol has no 404.
    let unknown = vec![("piece".to_owned(), "0".repeat(64))];
    let result = session
        .fetch(&target(dest, "/piece", unknown), Duration::from_secs(2))
        .await;
    assert!(
        matches!(result, Err(FetchError::Timeout)),
        "an unknown piece must surface a clean timeout, got {result:?}"
    );

    node_task.abort();
}
