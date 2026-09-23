//! N shared-instance clients register at once onto a half-duplex LoRa
//! interface, with only the interface's own spacing left to separate them.
//!
//! ## What this is for
//!
//! The core used to hold the first announce of every local-client
//! destination for `LOCAL_CLIENT_ANNOUNCE_DELAY_MS` (250 ms), "to batch
//! multiple registrations during startup". Removing it (2026-09-14, on the
//! deviation rule: wire and semantics held, but the third condition — a
//! measurable improvement of priority 1 — never had a measurement) raises
//! exactly one question worth asking: does a start-up burst now arrive at
//! the radio as a burst?
//!
//! Not as a burst at the *serial* boundary, which is what this test
//! measures. The RNode interface makes the frame that acquires an idle
//! channel serve the randomised wait its `ChannelAccess` policy draws
//! (DIFS plus a contention window, 48..360 ms at SF7/125 kHz) and spaces
//! every further frame of the same burst by a fixed `MIN_SPACING_MS`
//! (50 ms), so the frames reach the modem separated. The core's hold was a second, weaker copy
//! of that one layer too low — and a 250 ms hold cannot separate N
//! simultaneous registrations anyway, because it delays them all by the
//! same 250 ms.
//!
//! What this test does NOT show is that the frames are separated *on the
//! air*, and the name must not be read that way. 50 ms is the serial
//! floor (`MIN_SPACING_MS` is documented as exactly that), not an
//! airtime; at the hardware corpus' SF7 / BW 62.5 kHz every frame in the
//! band is far longer than that. Measured on the rig
//! (`lora_lncp_proof_retry`, run hardware_20260803T184800+0200): host TX
//! to peer RX is ~400 ms for an 86-byte frame, ~750 ms for 167 bytes and
//! ~1150 ms for 183 bytes. Two frames written 50 ms apart therefore both
//! sit in the firmware queue, and the sender stays deaf for whatever
//! airtime it ends up spending on them. Codeberg #187 is one instance:
//! an announce queued behind a priority link request went to serial 51 ms
//! after it, and the proof coming back was lost along with the announce —
//! both directions, one collision. The airtime-aware alternative
//! (`leviculum_core::rnode::compute_spacing_ms`) exists and has no caller;
//! wiring it in is Bug #25, attempted in c2eba153 and reverted in 12f99a02.
//!
//! "Leave back to back" is what this paragraph said until 2026-09-23, and
//! the hardware has since said otherwise: in `bench_single_pair_fast` of the
//! 2026-09-22 full run the far end listened through the 152 ms after the
//! first frame's airtime ended and heard nothing, so the firmware does
//! contend for the second frame rather than flushing it. That makes the
//! defect worse rather than better — the contention it runs is released by
//! the end of OUR frame, which is the same event that releases the far end's
//! answer, so the two draw against each other. The census is in
//! `burst_continuation_contends_inside_its_own_answer` (Codeberg #374).
//!
//! ## Topology
//!
//! ```text
//!   N clients (abstract unix socket, one announce each, back to back)
//!        │
//!        v
//!   lnsd (in-process, share_instance) ──► RNode channel interface
//!                                          │ KISS over an in-memory duplex
//!                                          v
//!                                     firmware stub, timestamping CMD_DATA
//! ```
//!
//! No hardware, no Docker, no Python: the "radio" is a duplex the stub owns,
//! and what is measured is the sequence of frames the interface hands it.
//!
//! Sans-hardware, deterministic, a few seconds.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use leviculum_core::framing::hdlc::frame as hdlc_frame;
use leviculum_core::framing::kiss::{self, KissDeframeResult, KissDeframer};
use leviculum_core::identity::Identity;
use leviculum_core::packet::{Packet, PacketType};
use leviculum_core::rnode;
use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::interfaces::{RNodeChannelFactory, RNodeChannelHalves, RNodeChannelOpenFuture};
use rand_core::OsRng;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Clients registering in the same instant — a shared instance coming up
/// with a messenger, a propagation client and a few tools attached.
const CLIENTS: usize = 5;

/// The interface's fixed spacing between queued frames
/// (`rnode::MIN_SPACING_MS`), less the slack a loaded test host can add to a
/// timer. What is being shown is that the frames are *separated*, not that
/// the separation is accurate to the millisecond.
const SPACING_FLOOR_MS: u128 = 40;

/// Hands leviculum one half of an in-memory duplex; the stub owns the other.
struct DuplexFactory(Mutex<Option<RNodeChannelHalves>>);

impl RNodeChannelFactory for DuplexFactory {
    fn open(&self) -> RNodeChannelOpenFuture {
        let taken = self.0.lock().expect("factory lock").take();
        Box::pin(async move { taken.ok_or_else(|| "channel already opened".into()) })
    }
}

/// One frame the "radio" was asked to transmit, and when.
struct AirFrame {
    at: Instant,
    payload: Vec<u8>,
}

/// A minimal RNode firmware: answers the detect probe and the radio
/// configuration, then timestamps every `CMD_DATA` it is handed.
async fn firmware_stub(mut peer: tokio::io::DuplexStream, air: Arc<Mutex<Vec<AirFrame>>>) {
    let mut deframer = KissDeframer::with_max_payload(rnode::HW_MTU);
    let mut buf = [0u8; 1024];
    let push = |reply: &mut Vec<u8>, cmd: u8, payload: &[u8]| {
        let mut one = Vec::new();
        kiss::frame(cmd, payload, &mut one);
        reply.extend_from_slice(&one);
    };
    loop {
        let n = match peer.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut reply: Vec<u8> = Vec::new();
        for f in deframer.process(&buf[..n]) {
            if let KissDeframeResult::Frame { command, payload } = f {
                match command {
                    rnode::CMD_DETECT => {
                        push(&mut reply, rnode::CMD_DETECT, &[rnode::DETECT_RESP]);
                        push(
                            &mut reply,
                            rnode::CMD_FW_VERSION,
                            &[rnode::REQUIRED_FW_MAJ, rnode::REQUIRED_FW_MIN],
                        );
                        push(&mut reply, rnode::CMD_PLATFORM, &[rnode::PLATFORM_ESP32]);
                        push(&mut reply, rnode::CMD_MCU, &[0x00]);
                    }
                    rnode::CMD_FREQUENCY
                    | rnode::CMD_BANDWIDTH
                    | rnode::CMD_TXPOWER
                    | rnode::CMD_SF
                    | rnode::CMD_CR
                    | rnode::CMD_RADIO_STATE => push(&mut reply, command, &payload),
                    rnode::CMD_DATA => air.lock().expect("air lock").push(AirFrame {
                        at: Instant::now(),
                        payload: payload.to_vec(),
                    }),
                    _ => {}
                }
            }
        }
        if !reply.is_empty() && peer.write_all(&reply).await.is_err() {
            return;
        }
    }
}

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

/// One client's registration announce, HDLC-framed for the IPC.
fn registration(index: usize) -> ([u8; 16], Vec<u8>) {
    let mut dest = Destination::new(
        Some(Identity::generate(&mut OsRng)),
        Direction::In,
        DestinationType::Single,
        "burstapp",
        &["client", "reg"],
    )
    .expect("destination");
    let hash = *dest.hash().as_bytes();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    let packet = dest
        .announce(
            Some(format!("burst{index}").as_bytes()),
            &mut OsRng,
            now_ms,
            now_ms / 1000,
        )
        .expect("announce");
    let mut raw = [0u8; 500];
    let len = packet.pack(&mut raw).expect("pack");
    let mut framed = Vec::new();
    hdlc_frame(&raw[..len], &mut framed);
    (hash, framed)
}

/// THE pin: a simultaneous registration burst reaches the radio complete and
/// separated. Complete, because nothing in the removal may lose an announce;
/// separated, because the interface — not the core — is what keeps a
/// half-duplex radio from being asked to transmit N frames at once.
#[tokio::test]
async fn a_registration_burst_reaches_a_half_duplex_radio_spaced_out() {
    let (port, peer) = tokio::io::duplex(64 * 1024);
    let (read_half, write_half) = tokio::io::split(port);
    let halves: Mutex<Option<RNodeChannelHalves>> = Mutex::new(Some((
        Box::new(read_half) as Box<dyn AsyncRead + Send + Unpin>,
        Box::new(write_half) as Box<dyn AsyncWrite + Send + Unpin>,
    )));

    let air = Arc::new(Mutex::new(Vec::<AirFrame>::new()));
    let stub = tokio::spawn(firmware_stub(peer, Arc::clone(&air)));

    let instance_name = format!("burst_{}", std::process::id());
    let storage = tempfile::tempdir().expect("storage");
    let mut lnsd = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.clone())
        // SF7/125 kHz: the fast end of LoRa, so the burst's airtime stays
        // well inside the interface's announce cap and what is measured is
        // the spacing rather than a cap suppressing frames.
        .add_rnode_channel_interface(
            Arc::new(DuplexFactory(halves)),
            868_000_000,
            125_000,
            7,
            5,
            17,
        )
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("lnsd builds");
    lnsd.start().await.expect("lnsd starts");

    // The radio has to be online before the burst, or the queue would be
    // measuring the lifecycle instead of the spacing.
    tokio::time::sleep(Duration::from_secs(2)).await;
    air.lock().expect("air lock").clear();

    // Every client attaches first; a socket that is still being accepted is
    // not yet a LocalClient interface, and the burst has to be a burst.
    let mut clients = Vec::new();
    for _ in 0..CLIENTS {
        clients.push(
            connect_abstract_unix(&instance_name).expect("client attaches to the shared instance"),
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The burst: every client announces, back to back, in the same instant.
    let mut hashes = Vec::new();
    for (index, client) in clients.iter_mut().enumerate() {
        let (hash, framed) = registration(index);
        client.write_all(&framed).await.expect("client writes");
        client.flush().await.expect("client flushes");
        hashes.push(hash);
    }

    // Long enough for the first frame's acquisition wait (48..360 ms at
    // SF7/125 kHz) plus CLIENTS × 50 ms of spacing, with room to spare.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Take the timestamps and let the stub's lock go before anything else
    // is awaited: what is analysed below is a snapshot, not live state.
    let air_times: Vec<Instant> = {
        let frames = air.lock().expect("air lock");
        frames
            .iter()
            .filter(|f| {
                Packet::unpack(&f.payload)
                    .map(|p| {
                        p.flags.packet_type == PacketType::Announce
                            && hashes.contains(&p.destination_hash)
                    })
                    .unwrap_or(false)
            })
            .map(|f| f.at)
            .collect()
    };

    let gaps: Vec<u128> = air_times
        .windows(2)
        .map(|w| w[1].duration_since(w[0]).as_millis())
        .collect();
    println!(
        "BURST_SPACING clients={CLIENTS} on_air={} gaps_ms={gaps:?}",
        air_times.len()
    );

    assert_eq!(
        air_times.len(),
        CLIENTS,
        "every registration must reach the radio — the removal may not lose \
         an announce: {} of {CLIENTS} on the air",
        air_times.len()
    );

    // The thing the core hold was presumably meant to prevent: N frames
    // handed to a half-duplex radio at once. The interface's own spacing is
    // what stops it, and it stops it whether or not the core held anything.
    for (i, gap) in gaps.iter().enumerate() {
        assert!(
            *gap >= SPACING_FLOOR_MS,
            "announces {i} and {} left the interface {gap} ms apart, below \
             the interface's own {} ms spacing — a half-duplex radio was \
             asked to transmit two frames at once. gaps={gaps:?}",
            i + 1,
            rnode::MIN_SPACING_MS
        );
    }

    lnsd.stop().await.ok();
    stub.abort();
}
