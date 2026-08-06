//! mvr for Codeberg #196 — the in-driver core processor seam answers a
//! same-tick event over a real interface.
//!
//! Constraint 3 of the batch: `PacketProofRequested`, `LinkProofRequested` and
//! `ResourceAdvertised` cannot be deferred to a later tick. The seam has to let
//! a processor answer them synchronously and have the answer transmitted on the
//! driver's own send path. The driver unit tests show the mechanism on a
//! synthetic `TickOutput`; this shows it end to end, on the wire.
//!
//! Topology: two in-process `ReticulumNode`s on `127.0.0.1` over TCP loopback.
//! A = TCP server + responder, built with `ProofStrategy::App`, so the core
//! emits `PacketProofRequested` and sends nothing itself. B = TCP client, which
//! installs A's path from its announce and sends one single packet.
//!
//! A registered processor on A answers that event in the same tick with
//! `send_proof_on_interface`. If the seam works, B sees
//! `PacketDeliveryConfirmed`. `no_processor_means_no_proof` is the negative
//! control on the identical scenario: with no processor registered, nothing
//! answers, and B must see no confirmation. Without that control the positive
//! result would not distinguish the seam from the core proving by itself.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::{Destination, DestinationType, Direction, Identity, ProofStrategy};
use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder, StdNodeCore};

fn next_port() -> u16 {
    crate::harness::port_alloc::free_tcp_port()
}

/// Answers `PacketProofRequested` in the tick that produced it.
///
/// This is the whole shape the seam exists for: take the event, call the core
/// method that answers it, hand the `TickOutput` back. No `.await`, no channel,
/// no call into the driver's async API — none of which this signature can
/// express.
struct ProofResponder {
    answered: Arc<AtomicUsize>,
}

impl CoreProcessor for ProofResponder {
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput {
        let NodeEvent::PacketProofRequested {
            packet_hash,
            destination_hash,
            interface_index,
        } = event
        else {
            return TickOutput::empty();
        };
        match core.send_proof_on_interface(packet_hash, destination_hash, *interface_index) {
            Ok(output) => {
                self.answered.fetch_add(1, Ordering::Relaxed);
                output
            }
            Err(e) => {
                eprintln!("mvr196: send_proof_on_interface failed: {e:?}");
                TickOutput::empty()
            }
        }
    }
}

struct Outcome {
    confirmed: bool,
    answered: usize,
}

/// One full announce → path-install → single-packet → proof cycle, with the
/// processor either registered or not.
async fn run_once(with_processor: bool) -> Outcome {
    let server_addr: SocketAddr = format!("127.0.0.1:{}", next_port()).parse().unwrap();
    let answered = Arc::new(AtomicUsize::new(0));

    // A: TCP server, application-decided proofs.
    let a_storage = tempfile::tempdir().expect("tempdir A");
    let mut a_builder = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_server(server_addr)
        .storage_path(a_storage.path().to_path_buf());
    if with_processor {
        a_builder = a_builder.core_processor(ProofResponder {
            answered: Arc::clone(&answered),
        });
    }
    let mut a = a_builder.build().await.expect("build A");
    a.start().await.expect("start A");

    // B: TCP client.
    let b_storage = tempfile::tempdir().expect("tempdir B");
    let mut b = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .add_tcp_client(server_addr)
        .storage_path(b_storage.path().to_path_buf())
        .build()
        .await
        .expect("build B");
    b.start().await.expect("start B");

    let mut b_rx = b.take_event_receiver().expect("B event rx");
    let (confirm_tx, mut confirm_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let b_drain = tokio::spawn(async move {
        while let Some(ev) = b_rx.recv().await {
            if matches!(ev, NodeEvent::PacketDeliveryConfirmed { .. }) {
                let _ = confirm_tx.send(());
            }
        }
    });

    // A registers and announces the destination B will send to.
    let a_identity = Identity::generate(&mut rand_core::OsRng);
    let a_public = Identity::from_public_key_bytes(&a_identity.public_key_bytes()).unwrap();
    let mut a_dest = Destination::new(
        Some(a_identity),
        Direction::In,
        DestinationType::Single,
        "mvr",
        &["seam196", "resp"],
    )
    .expect("A destination");
    // Per-destination, not a node default: `ProofStrategy::App` is what makes
    // the core emit `PacketProofRequested` and send nothing itself.
    a_dest.set_proof_strategy(ProofStrategy::App);
    let a_hash = *a_dest.hash();
    a.register_destination(a_dest);

    tokio::time::sleep(Duration::from_millis(500)).await;
    a.announce_destination(&a_hash, Some(b"seam196"))
        .await
        .expect("A announce");

    // B installs the path from the announce.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !b.has_path(&a_hash) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(b.has_path(&a_hash), "B never installed the path from A");
    b.remember_identity(a_hash, a_public);

    // One single packet, then wait for the proof to come back.
    b.packet_sender(&a_hash)
        .send(b"mvr196")
        .await
        .expect("B send");

    let confirmed = tokio::time::timeout(Duration::from_secs(5), confirm_rx.recv())
        .await
        .is_ok();

    b_drain.abort();
    let _ = b.stop().await;
    let _ = a.stop().await;

    Outcome {
        confirmed,
        answered: answered.load(Ordering::Relaxed),
    }
}

/// The seam answers `PacketProofRequested` inside the tick that produced it,
/// and the answer goes out on the driver's own send path — B gets the proof.
#[tokio::test]
async fn processor_answers_packet_proof_in_the_same_tick() {
    let outcome = run_once(true).await;
    assert_eq!(
        outcome.answered, 1,
        "the processor must have been handed exactly one PacketProofRequested"
    );
    assert!(
        outcome.confirmed,
        "B must receive PacketDeliveryConfirmed: the processor's TickOutput has \
         to be transmitted on the driver's own send path"
    );
}

/// Negative control on the identical scenario. `ProofStrategy::App` means the
/// core proves nothing by itself, so with no processor registered there is
/// nothing to answer the event and B must see no confirmation. This is what
/// makes the positive result above attributable to the seam.
#[tokio::test]
async fn no_processor_means_no_proof() {
    let outcome = run_once(false).await;
    assert_eq!(outcome.answered, 0);
    assert!(
        !outcome.confirmed,
        "with no processor, nothing answers PacketProofRequested and no proof \
         can reach B — if this fires, the positive test proves nothing"
    );
}
