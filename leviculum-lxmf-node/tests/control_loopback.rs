//! The remote-management control destination, in process, over TCP
//! loopback (leviculum#384 part 4): a raw client node runs the exact
//! flow `lxmd --status --remote` runs — resolve the node's identity from
//! its announce, derive `lxmf.propagation.control`, link, identify,
//! request — against lnpnd's production engine inside the helper.
//!
//! Positive and negative: the allowed identity gets the stats map and
//! the sync trigger's honest NOT_FOUND for an unknown peer; an identity
//! that was never allowed is refused in words, with the reference's
//! `ERROR_NO_ACCESS` (`LXMRouter.py:840`). It used to get silence, which
//! is what `RNS.Destination.ALLOW_LIST` does
//! (`reference/Reticulum/RNS/Link.py:867-874`) and which reaches an
//! operator as "timed out" — see `lnpnd/src/engine.rs`,
//! `register_control_handlers`, for why we answer instead. The
//! cross-stack versions of both directions are the conformance cell
//! `lxmf_pn_remote_mgmt.toml`.

mod common;

use std::time::Duration;

use common::{Helper, Setup, Wire};
use leviculum_core::{Destination, DestinationHash};
use leviculum_lxmf::control::{
    ControlResponse, CONTROL_ASPECTS, STATS_GET_PATH, SYNC_REQUEST_PATH,
};
use leviculum_lxmf::node::APP_NAME;
use leviculum_lxmf::PeerError;
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::ReticulumNode;

async fn client_node(hub_addr: std::net::SocketAddr, dir: &std::path::Path) -> ReticulumNode {
    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .storage_path(dir.to_path_buf())
        .add_tcp_client(hub_addr)
        .build()
        .await
        .expect("build client node");
    node.start().await.expect("start client node");
    node
}

fn parse_hash(raw: &str) -> [u8; 16] {
    let mut hash = [0u8; 16];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[2 * index..2 * index + 2], 16).expect("hex");
    }
    hash
}

/// Run one identified control request and return the raw response, or
/// `None` on the in-protocol timeout.
async fn control_request(
    node: &ReticulumNode,
    identity: &leviculum_core::Identity,
    pn_hash: [u8; 16],
    path: &str,
    data: Option<&[u8]>,
    timeout: Duration,
) -> Option<Vec<u8>> {
    // Identity recall needs the node announce; poll for it.
    let pn_dest = DestinationHash::new(pn_hash);
    let deadline = tokio::time::Instant::now() + timeout;
    let remote_identity = loop {
        if let Some(identity) = node.get_identity(&pn_dest) {
            break identity;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("the node's announce never arrived");
        }
        let _ = node.request_path(&pn_dest).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let name_hash = Destination::compute_name_hash(APP_NAME, &CONTROL_ASPECTS);
    let control_hash = Destination::compute_destination_hash(&name_hash, remote_identity.hash());
    assert!(
        node.wait_for_path(&control_hash, timeout, Duration::from_secs(2))
            .await
            .unwrap_or(false),
        "no path to the control destination — its announce never arrived"
    );
    let signing_key = remote_identity.ed25519_verifying().to_bytes();
    let (link, established) = node
        .connect_awaited(&control_hash, &signing_key)
        .await
        .expect("link setup");
    tokio::time::timeout(timeout, established)
        .await
        .expect("link establishes")
        .expect("link establishes cleanly");
    node.identify_link(link.link_id(), identity)
        .await
        .expect("identify");
    let (_, response) = node
        .send_request_awaited(link.link_id(), path, data, Some(timeout.as_millis() as u64))
        .await
        .expect("request sent");
    let outcome = tokio::time::timeout(timeout, response).await;
    let _ = node.close_link(link.link_id()).await;
    match outcome {
        Ok(Ok(info)) => Some(info.response_data),
        _ => None,
    }
}

#[tokio::test]
async fn an_allowed_identity_reads_stats_and_an_unknown_peer_is_not_found() {
    let mut hub = Helper::start(Setup {
        transport: true,
        ..Setup::new("pn-hub", Wire::listen_any())
    })
    .await;
    let hub_addr = hub.listen_addr();
    hub.command("pn_enable 1 stamp_cost=16 peering_cost=1");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let pn_hash = loop {
        hub.drain();
        if let Some(event) = hub.find("lxmf_pn_ready") {
            break parse_hash(event.field("hash").expect("hash"));
        }
        assert!(std::time::Instant::now() < deadline, "pn never ready");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let mut node = client_node(hub_addr, dir.path()).await;
    let identity = leviculum_std::generate_identity();
    let identity_hex: String = identity.hash().iter().map(|b| format!("{b:02x}")).collect();
    hub.command(&format!("pn_allow_control {identity_hex}"));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        hub.drain();
        if hub.seen("lxmf_pn_control_allowed") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "allow never confirmed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let response = control_request(
        &node,
        &identity,
        pn_hash,
        STATS_GET_PATH,
        None,
        Duration::from_secs(30),
    )
    .await
    .expect("the allowed identity must get an answer");
    match ControlResponse::decode(&response).expect("decodable") {
        ControlResponse::Stats(stats) => {
            assert_eq!(stats.destination_hash, pn_hash);
            assert_eq!(stats.target_stamp_cost, 16);
            assert_eq!(stats.peering_cost, 1);
            assert_eq!(stats.total_peers, 0);
            assert_eq!(stats.messagestore_count, 0);
            assert!(
                stats.messagestore_limit_bytes.unwrap_or(0) > 0,
                "lxmd's printer divides by the limit; it must be a number"
            );
        }
        other => panic!("expected the stats map, got {other:?}"),
    }

    // The sync trigger for a peer the node never had: the reference's
    // ERROR_NOT_FOUND (`peer_sync_request`, `LXMRouter.py:850`).
    let mut packed_peer = Vec::new();
    leviculum_lxmf::msgpack::bin(&mut packed_peer, &[0xEE; 16]);
    let response = control_request(
        &node,
        &identity,
        pn_hash,
        SYNC_REQUEST_PATH,
        Some(&packed_peer),
        Duration::from_secs(30),
    )
    .await
    .expect("the trigger must be answered");
    assert_eq!(
        ControlResponse::decode(&response).expect("decodable"),
        ControlResponse::Error(PeerError::NotFound)
    );

    // Negative: an identity nobody allowed is refused, and told so. Not
    // data, and not silence either — silence is indistinguishable from a
    // dead node on the client side.
    let stranger = leviculum_std::generate_identity();
    let response = control_request(
        &node,
        &stranger,
        pn_hash,
        STATS_GET_PATH,
        None,
        Duration::from_secs(6),
    )
    .await
    .expect("a refusal must be answered, not sat out as a timeout");
    assert_eq!(
        ControlResponse::decode(&response).expect("decodable"),
        ControlResponse::Error(PeerError::NoAccess),
        "an identity outside control_allowed must be refused in words, never served"
    );

    node.stop().await.expect("client stops");
}
