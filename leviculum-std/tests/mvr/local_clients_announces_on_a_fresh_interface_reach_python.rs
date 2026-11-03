//! mvr for order 1358: announces a shared instance forwards from its local
//! clients onto a fresh connection to a Python node.
//!
//! Field shape: 1307 G1 finding 2 and 1307d. A home's announce and its group
//! engine's announce leave lnsd back to back at registration, two local
//! clients each announcing one destination, and the Python peer on the far
//! side of lnsd's TCP link learned the first and not the second. The same
//! burst comes at every lnsd start with several clients attached (a phone
//! mailbox, lnpnd, a group engine, lnomad).
//!
//! The Python peer counts whatever arrives on its interface, and whether lnsd
//! made an announce or forwarded it for a client is not visible on the wire:
//! more than two arrivals above 3 Hz on an interface younger than two hours
//! hold every announce for an unknown destination
//! (`RNS/Transport.py:1695-1706`, `RNS/Interfaces/Interface.py:145-165`), as
//! in 1317's `a_third_announce_on_a_fresh_interface_waits_at_python`.
//!
//! Here: an in-process lnsd sharing its instance, with one TCP client to a
//! Python daemon, the connection up; then N raw IPC clients attach and write
//! one announce per destination, taking turns, back to back. Python must
//! learn every destination within 5 s. One client is the control.
//!
//! What it measured: green on the tree it was written against, which already
//! carried 1320. The 1307 measurement came from the home node branch before
//! 1320 reached it (bba0183c says so). A client's announce is stored at
//! hops 0, the IPC hop undone (`Transport::incoming_hop_count`), and its one
//! emission goes through `forward_on_all`, whose hops-0 gate puts it in the
//! fresh interface's pacer like the node's own (the core pin
//! `local_client_announces_are_paced_on_a_fresh_network_interface`). With
//! `FRESH_ANNOUNCE_SPACING_MS` set to 0 the three-client test goes red, the
//! third destination held at Python (its spawned interface reports
//! `held_announces=1 burst_active=true`), and the two-client test stays
//! green: two arrivals are not enough samples for the reference to measure.

use std::time::Duration;

use leviculum_core::framing::hdlc::frame as hdlc_frame;
use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;
use rand_core::OsRng;
use tokio::io::AsyncWriteExt;

use crate::harness::TestDaemon;

/// Connect to the shared instance the way any client does.
fn connect_abstract_unix(instance_name: &str) -> std::io::Result<tokio::net::UnixStream> {
    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(
        format!("rns/{instance_name}").as_bytes(),
    )?;
    let std_stream = std::os::unix::net::UnixStream::connect_addr(&addr)?;
    std_stream.set_nonblocking(true)?;
    tokio::net::UnixStream::from_std(std_stream)
}

/// One client's announce of a destination of its own, HDLC-framed for the
/// IPC, as a client puts it on the socket at registration.
fn client_announce(index: usize) -> ([u8; 16], Vec<u8>) {
    let mut dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "mvr1358",
        &["client", &index.to_string()],
    )
    .expect("destination");
    let hash = *dest.hash().as_bytes();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    let packet = dest
        .announce(None, &mut OsRng, now_ms, now_ms / 1000)
        .expect("announce");
    let mut raw = [0u8; 500];
    let len = packet.pack(&mut raw).expect("pack");
    let mut framed = Vec::new();
    hdlc_frame(&raw[..len], &mut framed);
    (hash, framed)
}

/// `clients` local clients announce `destinations` destinations between
/// them, taking turns, all back to back; returns the destinations Python had
/// not learned after 5 s.
async fn unknown_at_python(clients: usize, destinations: usize) -> Vec<[u8; 16]> {
    let daemon = TestDaemon::start().await.expect("start daemon");

    let instance_name = format!("mvr1358_{}_{clients}_{destinations}", std::process::id());
    let storage = tempfile::tempdir().expect("storage");
    let mut lnsd = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .share_instance(true)
        .instance_name(instance_name.clone())
        .add_tcp_client(daemon.rns_addr())
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("lnsd builds");
    lnsd.start().await.expect("lnsd starts");
    lnsd.wait_for_interfaces_ready(Duration::from_secs(5))
        .await
        .expect("the TCP client connects");

    // Every client attaches first, so what follows is one burst.
    let mut sockets = Vec::new();
    for _ in 0..clients {
        sockets.push(connect_abstract_unix(&instance_name).expect("client attaches"));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut frames = Vec::new();
    let mut hashes = Vec::new();
    for index in 0..destinations {
        let (hash, framed) = client_announce(index);
        hashes.push(hash);
        frames.push((index % clients, framed));
    }
    for (client, framed) in frames {
        sockets[client].write_all(&framed).await.expect("write");
        sockets[client].flush().await.expect("flush");
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut unknown = hashes;
    while tokio::time::Instant::now() < deadline && !unknown.is_empty() {
        let mut still = Vec::new();
        for hash in unknown {
            if !daemon.has_path(hash).await {
                still.push(hash);
            }
        }
        unknown = still;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !unknown.is_empty() {
        for iface in daemon.get_interfaces().await.unwrap_or_default() {
            println!(
                "PYTHON_IFACE name={:?} held_announces={} burst_active={}",
                iface.name, iface.held_announces, iface.burst_active
            );
        }
    }
    lnsd.stop().await.ok();
    unknown
}

fn hex_list(hashes: &[[u8; 16]]) -> Vec<String> {
    hashes.iter().map(hex::encode).collect()
}

/// THE pin: two local clients, one announce each, and Python learns both.
#[tokio::test]
async fn two_local_clients_announces_both_reach_python() {
    let unknown = unknown_at_python(2, 2).await;
    assert!(
        unknown.is_empty(),
        "Python did not learn {} of two client destinations within 5 s: {:?}",
        unknown.len(),
        hex_list(&unknown)
    );
}

/// Three clients, as at an lnsd start with a mailbox, lnpnd and a group
/// engine attached.
#[tokio::test]
async fn three_local_clients_announces_all_reach_python() {
    let unknown = unknown_at_python(3, 3).await;
    assert!(
        unknown.is_empty(),
        "Python did not learn {} of three client destinations within 5 s: {:?}",
        unknown.len(),
        hex_list(&unknown)
    );
}

/// The 1320 pin `five_destinations_on_a_fresh_interface_all_arrive_at_python`
/// (`a_third_announce_on_a_fresh_interface_waits_at_python.rs`) with the five
/// coming from two local clients instead of the node itself.
#[tokio::test]
async fn five_destinations_from_two_local_clients_all_arrive_at_python() {
    let unknown = unknown_at_python(2, 5).await;
    assert!(
        unknown.is_empty(),
        "Python did not learn {} of five client destinations within 5 s: {:?}",
        unknown.len(),
        hex_list(&unknown)
    );
}

/// The control: one client, one announce. What holds the others above is
/// the burst, not the IPC path or the connection.
#[tokio::test]
async fn control_one_local_client_announce_reaches_python() {
    let unknown = unknown_at_python(1, 1).await;
    assert!(
        unknown.is_empty(),
        "Python did not learn the one client destination within 5 s: {:?}",
        hex_list(&unknown)
    );
}
