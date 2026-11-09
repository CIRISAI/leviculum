//! Direct UDP interface: one punched socket, one peer (+ciris, leviculum#70).
//!
//! What a successful direct-link punch leaves behind. The socket is the one
//! that probed the facilitator and punched, so it owns the NAT mapping the
//! peer is sending to; the interface keeps that mapping alive and carries
//! Reticulum packets over it, one datagram per packet, like `UDPInterface`.
//!
//! Beyond a plain UDP interface it:
//! - accepts datagrams only from the punched peer address;
//! - keeps the pinhole open with a keepalive punch frame every
//!   [`KEEPALIVE_INTERVAL`], and goes down after [`INACTIVITY_TIMEOUT`] with
//!   nothing heard (the peer sends keepalives too, so silence means the path
//!   or the peer is gone);
//! - (+ciris) with a peer that runs the two-sided commit, probes every
//!   [`FAST_KEEPALIVE_INTERVAL`] with a sequenced punch frame the peer
//!   acks, and goes down after [`FAST_INACTIVITY_TIMEOUT`] of silence: a
//!   dead path is left in seconds, as ICE consent checks (RFC 7675) do,
//!   instead of minutes. With any other peer (rns-rs) the plain keepalive
//!   and timeout above stay, since its interface does not ack keepalives;
//! - answers a late punch frame with its ack. The peer finishes punching
//!   only once it has seen an ack; if this side finished first and the one
//!   ack it sent was lost, the peer would otherwise punch into silence and
//!   fail while this side believed the path was up.
//!
//! It is leaf-only (`transit: false`): it exists for one peer pair, and this
//! node relays nobody else's traffic across it.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::direct_link::wire::{self, PunchKind, SessionId};
use leviculum_core::direct_link::DIRECT_LINK_MTU;
use leviculum_core::transport::InterfaceId;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket};

/// How often the pinhole is refreshed. Common NAT UDP mapping lifetimes run
/// 30 to 120 s; matches rns-rs.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Silence after which the direct path is declared dead (four missed
/// keepalives).
pub(crate) const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(120);
/// +ciris: the acked probe interval, and the silence after which the path
/// is dead (three missed probes, each answered by an ack and matched by the
/// peer's own), with a peer that acks them.
pub(crate) const FAST_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const FAST_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest datagram accepted: anything UDP can carry. The link is lowered to
/// `DIRECT_LINK_MTU` when it moves here, but a part sized for the link's
/// earlier, larger MTU may still be in flight, and a truncated datagram is
/// a silently lost packet.
const DATAGRAM_MAX: usize = 65_535;
const CHANNEL_DEPTH: usize = 256;

/// Timing knobs, overridable in tests.
#[derive(Clone, Copy)]
pub(crate) struct DirectUdpTiming {
    pub keepalive: Duration,
    pub inactivity: Duration,
    /// +ciris: keepalives carry a sequence number, so the peer acks them.
    pub sequenced: bool,
}

impl DirectUdpTiming {
    /// +ciris: acked probes, for a peer that runs the two-sided commit.
    pub(crate) fn fast() -> Self {
        DirectUdpTiming {
            keepalive: FAST_KEEPALIVE_INTERVAL,
            inactivity: FAST_INACTIVITY_TIMEOUT,
            sequenced: true,
        }
    }
}

impl Default for DirectUdpTiming {
    fn default() -> Self {
        DirectUdpTiming {
            keepalive: KEEPALIVE_INTERVAL,
            inactivity: INACTIVITY_TIMEOUT,
            sequenced: false,
        }
    }
}

/// Bring a punched socket up as an interface.
///
/// Returns the handle for the event loop and an abort handle for the I/O
/// task; aborting it drops the incoming channel, which the loop reads as the
/// interface disconnecting.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_direct_udp_interface(
    id: InterfaceId,
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    session: SessionId,
    token: [u8; 32],
    early: Vec<Vec<u8>>,
    timing: DirectUdpTiming,
    hw_mtu: Option<u32>,
) -> (InterfaceHandle, tokio::task::AbortHandle) {
    let name = format!(
        "DirectPeer/{}",
        session[..4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let (incoming_tx, incoming_rx) = mpsc::channel(CHANNEL_DEPTH);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(CHANNEL_DEPTH);
    let counters = Arc::new(InterfaceCounters::new());

    let task = tokio::spawn(direct_io_task(
        name.clone(),
        socket,
        peer,
        session,
        token,
        early,
        timing,
        incoming_tx,
        outgoing_rx,
        Arc::clone(&counters),
    ));

    let handle = InterfaceHandle {
        info: InterfaceInfo {
            id,
            name,
            // Signals what the path-MTU probe found (+ciris, leviculum#104):
            // `DIRECT_LINK_MTU` or a smaller step, or nothing (base 500)
            // when no padded probe was acked. Unlike a UDPInterface
            // (Codeberg #357, which signals none because the reference's
            // does not) this one only exists between two nodes that ran the
            // DirectLink upgrade, so its far end is never a Python node.
            // Signalling none held a link BORN here (a derived one-hop
            // address, never announced) at 500 on a path that carries 1400;
            // a link that moves here is lowered to `DIRECT_LINK_MTU` as
            // before, by both ends.
            hw_mtu: hw_mtu.map(|m| m.min(DIRECT_LINK_MTU)),
            is_local_client: false,
            bitrate: None,
            announce_cap_bitrate: None,
            tx_jitter_max_ms: None,
            tx_hold_spread_max_ms: None,
            acquisition: None,
            frame_turnaround_ms: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
            kind: leviculum_core::traits::InterfaceKind::Udp,
            ingress_control: Some(false),
            transit: false,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        ready: super::ReadySignal::ready_immediate(),
        teardown: None,
        next_tag: None,
    };
    (handle, task.abort_handle())
}

#[allow(clippy::too_many_arguments)]
async fn direct_io_task(
    name: String,
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    session: SessionId,
    token: [u8; 32],
    early: Vec<Vec<u8>>,
    timing: DirectUdpTiming,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    counters: Arc<InterfaceCounters>,
) {
    // What the peer sent while this end was still punching comes first.
    for data in early {
        counters
            .rx_bytes
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        if incoming_tx
            .send(IncomingPacket {
                data,
                peer: None,
                radio: None,
            })
            .await
            .is_err()
        {
            return;
        }
    }
    let peer = crate::direct_link::canonical(peer);
    let send_to = crate::direct_link::on_socket(&socket, peer);
    let mut probe_seq: u32 = 0;
    let mut buf = vec![0u8; DATAGRAM_MAX];
    let mut last_heard = Instant::now();
    let mut keepalive_tick = tokio::time::interval(timing.keepalive);
    keepalive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                let (len, src) = match result {
                    Ok(r) => r,
                    Err(e) => {
                        // ICMP unreachable surfaces here on some platforms as
                        // a one-off error; the inactivity timer decides death.
                        tracing::debug!("{name}: recv error: {e}");
                        continue;
                    }
                };
                if crate::direct_link::canonical(src) != peer {
                    continue;
                }
                let frame = &buf[..len];
                // +ciris (leviculum#104): the peer probing the path's MTU.
                if wire::is_mtu_probe(frame) {
                    if let Some(seq) = wire::parse_mtu_probe(frame, &session, &token) {
                        last_heard = Instant::now();
                        let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                        let _ = socket.send_to(&ack, send_to).await;
                    }
                    continue;
                }
                if wire::punch_frame_kind(frame).is_some() {
                    // Only a frame carrying this session's token is the peer
                    // speaking; a stale or forged one must not keep a dead
                    // path looking alive.
                    if let Some((kind, seq)) = wire::parse_punch_frame(frame, &session, &token) {
                        last_heard = Instant::now();
                        if kind == PunchKind::Punch && seq != wire::KEEPALIVE_SEQ {
                            let ack = wire::punch_frame(PunchKind::Ack, &session, &token, seq);
                            let _ = socket.send_to(&ack, send_to).await;
                        }
                    }
                    continue;
                }
                last_heard = Instant::now();
                if len == 0 {
                    continue;
                }
                counters.rx_bytes.fetch_add(len as u64, Ordering::Relaxed);
                if incoming_tx
                    .send(IncomingPacket {
                        data: frame.to_vec(),
                        peer: None,
                        radio: None,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            packet = outgoing_rx.recv() => {
                let Some(packet) = packet else {
                    // The loop dropped the handle: the interface was removed.
                    return;
                };
                match socket.send_to(&packet.data, send_to).await {
                    Ok(n) => {
                        counters.tx_bytes.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    Err(e) => tracing::debug!("{name}: send error: {e}"),
                }
            }
            _ = keepalive_tick.tick() => {
                if last_heard.elapsed() >= timing.inactivity {
                    tracing::info!(
                        "{name}: nothing from {peer} for {:?}, direct path down",
                        timing.inactivity
                    );
                    return;
                }
                // Sequenced, the peer acks it, so silence means the path is
                // gone even while the link has nothing to say.
                let seq = if timing.sequenced {
                    probe_seq = probe_seq.wrapping_add(1) % wire::KEEPALIVE_SEQ;
                    probe_seq
                } else {
                    wire::KEEPALIVE_SEQ
                };
                let keepalive = wire::punch_frame(PunchKind::Punch, &session, &token, seq);
                let _ = socket.send_to(&keepalive, send_to).await;
            }
        }
    }
}
