//! A Python initiator that lost its TCP connection and came back from
//! another address links to us again, without a new announce (leviculum
//! 1412).
//!
//! On miauhaus on 2026-11-04 a Fairphone running Columba (Python RNS)
//! asked the home for links at up to 235 a minute; the home proved every
//! request within milliseconds, the proofs reached the phone's socket, and
//! no link ever came up until Columba's RNS was restarted. The phone had
//! changed networks, so its TCP connection to the entry server dropped and
//! came back from another address. This pins our half of that: after such
//! a reconnect a Python `TCPClientInterface` (which reconnects in place,
//! `RNS/Interfaces/TCPInterface.py:80` `RECONNECT_WAIT`) reaches us on a
//! new connection, our TCP server hands it a new interface, the link request
//! arrives there, and our proof must go back on it and be accepted by
//! Python's `Link.validate_proof` (`RNS/Link.py:396-451`). A red here would
//! be ours; a green says a stuck peer after a reconnect is held in the
//! peer's own state.
//!
//! ```text
//!   Python daemon (TCPClientInterface) ──> cut-able proxy ──> our TCP server
//!   (initiator, create_link)                                  (link-accepting
//!                                                              destination)
//! ```
//!
//! Run: `cargo test -p leviculum-std --test rnsd_interop python_reconnect_link`

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand_core::OsRng;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationType, Direction, ProofStrategy};

use crate::common::{
    create_link_raw, temp_storage, wait_for_node_reannounce_on_daemon,
    wait_for_responder_established_link,
};
use crate::harness::TestDaemon;

/// A TCP forwarder whose connections can all be cut at once, as a phone
/// changing networks cuts its own; the listener stays, so the client comes
/// back through it on a new connection.
struct Proxy {
    addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
    pipes: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Proxy {
    async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let addr = listener.local_addr().expect("proxy addr");
        let accepted = Arc::new(AtomicUsize::new(0));
        let pipes: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let (count, held) = (Arc::clone(&accepted), Arc::clone(&pipes));
        tokio::spawn(async move {
            while let Ok((mut downstream, _)) = listener.accept().await {
                let Ok(mut up) = TcpStream::connect(upstream).await else {
                    continue;
                };
                count.fetch_add(1, Ordering::SeqCst);
                let pipe = tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut downstream, &mut up).await;
                });
                held.lock().expect("pipes").push(pipe);
            }
        });
        Self {
            addr,
            accepted,
            pipes,
        }
    }

    /// Drop every open connection: both ends see the close.
    fn cut(&self) {
        for pipe in self.pipes.lock().expect("pipes").drain(..) {
            pipe.abort();
        }
    }

    async fn wait_for_connections(&self, n: usize, budget: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        while tokio::time::Instant::now() < deadline {
            if self.accepted.load(Ordering::SeqCst) >= n {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }
}

#[tokio::test]
async fn python_reconnect_link_a_python_initiator_back_from_another_address_links_again() {
    let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind server");
    let server_addr = server.local_addr().expect("server addr");
    let storage = temp_storage("python_reconnect_link", "home");
    let mut home = leviculum_std::driver::ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server_listener(server)
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build home");
    let mut events = home.take_event_receiver().expect("events");
    home.start().await.expect("start home");

    let proxy = Proxy::start(server_addr).await;
    let daemon = TestDaemon::start().await.expect("start daemon");
    daemon
        .add_client_interface("127.0.0.1", proxy.addr.port(), Some("phone_uplink"))
        .await
        .expect("python client interface");
    assert!(
        proxy.wait_for_connections(1, Duration::from_secs(15)).await,
        "the Python client never connected"
    );

    let identity = Identity::generate(&mut OsRng);
    let key = hex::encode(identity.public_key_bytes());
    let mut destination = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "reconnect",
        &["home"],
    )
    .expect("destination");
    destination.set_accepts_links(true);
    destination.set_proof_strategy(ProofStrategy::All);
    let hash = *destination.hash();
    let hash_hex = hex::encode(hash.as_bytes());
    home.register_destination(destination);
    assert!(
        wait_for_node_reannounce_on_daemon(
            &daemon,
            &hash,
            &home,
            b"reconnect-home",
            Duration::from_secs(20)
        )
        .await,
        "Python never learned the path to us"
    );

    // Positive control: a link before the cut.
    let first = {
        let (cmd, h, k) = (daemon.cmd_addr(), hash_hex.clone(), key.clone());
        tokio::spawn(async move { create_link_raw(cmd, &h, &k, 20).await })
    };
    assert!(
        wait_for_responder_established_link(&mut events, Duration::from_secs(20))
            .await
            .is_some(),
        "we never established the first link"
    );
    first
        .await
        .expect("first create_link task")
        .expect("Python accepted our proof before the cut");

    // The phone changes networks: its connection drops and comes back from
    // another source address. No announce follows.
    proxy.cut();
    assert!(
        proxy.wait_for_connections(2, Duration::from_secs(30)).await,
        "the Python client never reconnected"
    );

    let second = {
        let (cmd, h, k) = (daemon.cmd_addr(), hash_hex.clone(), key.clone());
        tokio::spawn(async move { create_link_raw(cmd, &h, &k, 20).await })
    };
    assert!(
        wait_for_responder_established_link(&mut events, Duration::from_secs(25))
            .await
            .is_some(),
        "the link request after the reconnect never completed on our side"
    );
    second
        .await
        .expect("second create_link task")
        .expect("Python must accept our proof after the reconnect");
}
