//! AutoInterface orchestrator: peer discovery and lifecycle management
//!
//! Single tokio task that handles multicast discovery, peer management,
//! and data socket demultiplexing for AutoInterface.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use super::{
    bind_data_socket, bind_multicast_socket, bind_unicast_socket, build_discovery_packet,
    derive_multicast_address, enumerate_nics, group_name_tag, make_discovery_token,
    parse_discovery_packet, recv_from_any_by, unicast_discovery_port, verify_discovery_token,
    AdoptedNic, AutoInterfaceConfig, DeduplicationCache, ANNOUNCE_INTERVAL_SECS, AUTO_HW_MTU,
    DISCOVERY_PACKET_SIZE, MCAST_ECHO_TIMEOUT_SECS, NIC_RETRY_INTERVAL_SECS, NIC_WAIT_WARN_EVERY,
    NONCE_SIZE, PEERING_TIMEOUT_SECS, PEER_JOB_INTERVAL_SECS,
};
use crate::interfaces::{
    IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket,
};
use leviculum_core::transport::InterfaceId;

/// Maximum datagram size for AutoInterface (matches Python HW_MTU = 1196).
const MAX_DATAGRAM_SIZE: usize = 1196;

/// Per-peer channel buffer size
const PEER_CHANNEL_BUFFER: usize = 64;

/// Information about a discovered peer
struct PeerInfo {
    /// NIC name where this peer was discovered
    nic_name: String,
    /// NIC interface index (scope_id for SocketAddrV6)
    scope_id: u32,
    /// Last time we heard from this peer (discovery or data)
    last_heard: Instant,
    /// Last time we sent a reverse peering token to this peer
    last_reverse_peering: Instant,
    /// Channel to push incoming data to the event loop.
    /// Dropping this triggers handle_interface_down cascade.
    incoming_tx: mpsc::Sender<IncomingPacket>,
    /// Shared I/O counters (same Arc is in the InterfaceHandle)
    counters: Arc<InterfaceCounters>,
}

/// Everything one NIC owns: its identity, its three sockets, and the
/// discovery material derived from them.
///
/// This is what the orchestrator used to carry as six containers indexed by
/// the same `socket_index` — three socket vecs plus `active_nics`,
/// `our_tokens`, `discovery_packets`, and a `nic_states` map keyed by name.
/// Keeping them in step needed a helper whose only job was to skip the same
/// entries in all of them (`build_active_nics_and_tokens`), and that
/// alignment held only because nothing was ever added or removed. With one
/// `Nic` per NIC a failed bind is simply an entry that does not exist, and
/// there is no positional index left to fall out of step.
pub(crate) struct Nic {
    /// Interface name (e.g. "eth0", "wlan0")
    name: String,
    /// OS interface index, used as `scope_id` when addressing this NIC
    index: u32,
    /// Multicast discovery socket. Also sends reverse peering tokens, since
    /// it is already bound to this interface.
    mcast: UdpSocket,
    /// Unicast discovery socket
    unicast: UdpSocket,
    /// Data socket, shared with the send task of every peer on this NIC
    data: Arc<UdpSocket>,
    /// Our discovery token on this NIC: `hash(group_id + our link-local)`.
    /// A peer verifies it against the source address of our datagrams, so it
    /// is a property of the NIC and is derived once, when the NIC is bound.
    token: [u8; 32],
    /// The announce datagram we put on the wire on this NIC: token, instance
    /// nonce, data port. Constant for the life of the NIC.
    discovery_packet: [u8; DISCOVERY_PACKET_SIZE],
    /// Last time a multicast echo was received on this NIC (carrier detection)
    last_echo: Option<Instant>,
    /// Whether the NIC is currently timed out (no carrier)
    timed_out: bool,
}

/// The three sockets one NIC needs, before they are folded into a [`Nic`].
pub(crate) struct NicSockets {
    mcast: UdpSocket,
    unicast: UdpSocket,
    data: UdpSocket,
}

impl Nic {
    /// Fold one enumerated NIC and its three bound sockets into the single
    /// value that describes it, deriving the discovery material on the way.
    fn new(
        adopted: &AdoptedNic,
        sockets: NicSockets,
        group_id: &[u8],
        instance_nonce: &[u8; NONCE_SIZE],
        data_port: u16,
    ) -> Self {
        let token = make_discovery_token(group_id, &adopted.link_local.to_string());
        Nic {
            name: adopted.name.clone(),
            index: adopted.index,
            mcast: sockets.mcast,
            unicast: sockets.unicast,
            data: Arc::new(sockets.data),
            token,
            discovery_packet: build_discovery_packet(&token, instance_nonce, data_port),
            last_echo: None,
            timed_out: false,
        }
    }
}

/// Bind the three sockets each enumerated NIC needs, keeping only the NICs
/// where all three succeeded, in enumeration order.
///
/// `bind` is the seam: everything that touches the OS sits behind it, so the
/// property that used to need `build_active_nics_and_tokens` — a NIC whose
/// bind failed must not shift the NICs after it — stays testable without a
/// NIC. It cannot shift them any more: what comes out is one self-contained
/// `Nic` per NIC that came up, and nothing outside it describes that NIC.
fn bind_nics_with<F>(
    adopted: &[AdoptedNic],
    group_id: &[u8],
    instance_nonce: &[u8; NONCE_SIZE],
    data_port: u16,
    mut bind: F,
) -> Vec<Nic>
where
    F: FnMut(&AdoptedNic) -> Option<NicSockets>,
{
    adopted
        .iter()
        .filter_map(|nic| {
            let sockets = bind(nic)?;
            Some(Nic::new(nic, sockets, group_id, instance_nonce, data_port))
        })
        .collect()
}

/// The discovery token for a reverse peering announcement to a peer.
///
/// The token must be verifiable by the peer using the sender's source IP, so
/// it is OUR token on the NIC the peer was discovered on — the peer checks
/// `hash(group_id + our_source_ip)` — and never anything derived from the
/// peer's own address.
///
/// Returns `None` if the peer's NIC is not among `nics`.
pub(crate) fn compute_reverse_peering_token(peer_nic_name: &str, nics: &[Nic]) -> Option<[u8; 32]> {
    nics.iter()
        .find(|n| n.name == peer_nic_name)
        .map(|n| n.token)
}

/// Spawn the AutoInterface orchestrator as a background tokio task.
///
/// The orchestrator enumerates NICs, binds sockets, and runs the discovery
/// and data forwarding loop. Discovered peers are registered as individual
/// interfaces via `new_iface_tx`.
///
/// Returns a `watch::Receiver<usize>` that broadcasts the current peer count.
pub(crate) fn spawn_auto_interface(
    next_id: Arc<AtomicUsize>,
    new_iface_tx: mpsc::Sender<InterfaceHandle>,
    config: AutoInterfaceConfig,
) -> watch::Receiver<usize> {
    let (peer_count_tx, peer_count_rx) = watch::channel(0usize);
    tokio::spawn(async move {
        if let Err(e) = run_auto_interface(config, next_id, new_iface_tx, peer_count_tx).await {
            tracing::error!("AutoInterface orchestrator exited with error: {}", e);
        }
    });
    peer_count_rx
}

/// Log a failed per-NIC bind loudly the first time and quietly afterwards.
///
/// The first round is the one an operator reads after a failed start. Every
/// round after it repeats the same reason every `NIC_RETRY_INTERVAL_SECS`
/// until the condition clears, so it belongs at debug, otherwise a daemon
/// whose data port is permanently taken writes a warn line forever.
fn log_bind_failure(first_attempt: bool, nic: &str, socket_kind: &str, e: &io::Error) {
    if first_attempt {
        tracing::warn!(
            "AutoInterface: failed to bind {} on {}: {}",
            socket_kind,
            nic,
            e
        );
    } else {
        tracing::debug!(
            "AutoInterface: failed to bind {} on {} (re-check): {}",
            socket_kind,
            nic,
            e
        );
    }
}

/// One attempt to bring the interface up: enumerate NICs, then bind the three
/// sockets each of them needs.
///
/// `None` means nothing usable came out of this round, either because no NIC
/// passed enumeration or because none of them got a full socket set. Neither
/// is a permanent verdict: a NIC that is absent at boot is present seconds
/// later (USB Ethernet enumerating, WiFi associating, a bridge coming up),
/// and a link-local address still tentative under duplicate address detection
/// refuses a bind that succeeds on the next round. The caller treats `None`
/// as "wait and look again".
fn try_bind_nics(
    config: &AutoInterfaceConfig,
    mcast_addr: &Ipv6Addr,
    unicast_port: u16,
    instance_nonce: &[u8; NONCE_SIZE],
    attempt: u64,
) -> Option<Vec<Nic>> {
    let first_attempt = attempt == 1;

    let adopted = enumerate_nics(config);
    if adopted.is_empty() {
        return None;
    }

    let nics = bind_nics_with(
        &adopted,
        &config.group_id,
        instance_nonce,
        config.data_port,
        |nic| {
            // A NIC needs all three sockets or none: returning `None` drops the
            // ones bound so far, which is what the pop-what-we-pushed dance in
            // the parallel-vec version was for.
            let mcast = match bind_multicast_socket(
                nic,
                mcast_addr,
                config.discovery_port,
                &config.discovery_scope,
                config.multicast_loopback,
            ) {
                Ok(s) => {
                    tracing::info!(
                        "AutoInterface: multicast socket on {} ({})",
                        nic.name,
                        nic.link_local
                    );
                    s
                }
                Err(e) => {
                    log_bind_failure(first_attempt, &nic.name, "multicast", &e);
                    return None;
                }
            };

            let unicast = match bind_unicast_socket(nic, unicast_port) {
                Ok(s) => s,
                Err(e) => {
                    log_bind_failure(first_attempt, &nic.name, "unicast", &e);
                    return None;
                }
            };

            let data = match bind_data_socket(nic, config.data_port) {
                Ok(s) => s,
                Err(e) => {
                    log_bind_failure(first_attempt, &nic.name, "data", &e);
                    return None;
                }
            };

            Some(NicSockets {
                mcast,
                unicast,
                data,
            })
        },
    );

    if nics.is_empty() {
        return None;
    }

    tracing::info!(
        "AutoInterface: {} NIC(s), multicast={}, discovery_port={}, data_port={}",
        nics.len(),
        mcast_addr,
        config.discovery_port,
        config.data_port
    );

    Some(nics)
}

/// Retry `setup` every `retry_interval` until it yields a usable set of NICs,
/// or the event loop shuts down.
///
/// An empty NIC list at startup is a waiting state, not an exit. The daemon
/// routinely starts before the network does, and a task that returns here is
/// a node that stays deaf on the LAN for the rest of its life with one warn
/// line as the only trace. `setup` receives the 1-based attempt number so it
/// can keep its own logging from repeating forever, and the instance nonce,
/// which every NIC it binds needs to build its announce datagram.
///
/// Returns `None` only when the event loop is gone, which is the one case
/// where giving up is right, and the only one that says so in the log.
async fn wait_for_usable_nics<F>(
    mut setup: F,
    instance_nonce: &[u8; NONCE_SIZE],
    retry_interval: Duration,
    shutdown: &mpsc::Sender<InterfaceHandle>,
) -> Option<Vec<Nic>>
where
    F: FnMut(u64, &[u8; NONCE_SIZE]) -> Option<Vec<Nic>>,
{
    let started = Instant::now();
    let mut attempts: u64 = 0;

    loop {
        attempts += 1;
        if let Some(bound) = setup(attempts, instance_nonce) {
            if attempts > 1 {
                tracing::info!(
                    "AutoInterface: a usable network interface appeared after {:.0}s \
                     ({} checks), coming up now",
                    started.elapsed().as_secs_f64(),
                    attempts
                );
            }
            return Some(bound);
        }

        // "Still looking" and "gave up" have to read differently: today an
        // operator sees one warn line and cannot tell which one happened.
        if attempts == 1 {
            tracing::warn!(
                "AutoInterface: no usable network interface yet, not giving up, \
                 re-checking every {:.0}s until one appears",
                retry_interval.as_secs_f64()
            );
        } else if attempts.is_multiple_of(NIC_WAIT_WARN_EVERY) {
            tracing::warn!(
                "AutoInterface: still no usable network interface after {:.0}s \
                 ({} checks), still looking",
                started.elapsed().as_secs_f64(),
                attempts
            );
        } else {
            tracing::debug!(
                "AutoInterface: no usable network interface on check {}, still looking",
                attempts
            );
        }

        tokio::select! {
            _ = tokio::time::sleep(retry_interval) => {}
            _ = shutdown.closed() => {
                tracing::info!(
                    "AutoInterface: event loop shut down while waiting for a network \
                     interface, giving up"
                );
                return None;
            }
        }
    }
}

/// Main orchestrator loop.
///
/// Waits for a usable NIC, binds sockets, and runs the discovery + data loop.
/// Each discovered peer becomes a separate InterfaceHandle registered
/// via `new_iface_tx` into the main event loop.
async fn run_auto_interface(
    config: AutoInterfaceConfig,
    next_id: Arc<AtomicUsize>,
    new_iface_tx: mpsc::Sender<InterfaceHandle>,
    peer_count_tx: watch::Sender<usize>,
) -> io::Result<()> {
    let mcast_addr = derive_multicast_address(
        &config.group_id,
        &config.discovery_scope,
        config.multicast_address_type,
    )?;
    let unicast_port = unicast_discovery_port(config.discovery_port);

    let setup_config = config.clone();
    let setup = move |attempt: u64, nonce: &[u8; NONCE_SIZE]| {
        try_bind_nics(&setup_config, &mcast_addr, unicast_port, nonce, attempt)
    };

    run_auto_interface_with(
        config,
        next_id,
        new_iface_tx,
        peer_count_tx,
        mcast_addr,
        setup,
        Duration::from_secs_f64(NIC_RETRY_INTERVAL_SECS),
    )
    .await
}

/// The orchestrator with its bring-up step injected.
///
/// `setup` is the seam: everything that needs a real NIC (enumeration plus the
/// three binds) lives behind it, so the wait-then-come-up behaviour is
/// testable on a host that has no suitable NIC at all.
async fn run_auto_interface_with<F>(
    config: AutoInterfaceConfig,
    next_id: Arc<AtomicUsize>,
    new_iface_tx: mpsc::Sender<InterfaceHandle>,
    peer_count_tx: watch::Sender<usize>,
    mcast_addr: Ipv6Addr,
    setup: F,
    retry_interval: Duration,
) -> io::Result<()>
where
    F: FnMut(u64, &[u8; NONCE_SIZE]) -> Option<Vec<Nic>>,
{
    let unicast_port = unicast_discovery_port(config.discovery_port);
    // Per-group tag appended to peer interface names so peers reachable in
    // multiple groups do not collide in the registry / rnstatus. `None` for
    // the default group keeps single-section naming unchanged.
    let group_tag = group_name_tag(&config.group_id);

    // Generate per-instance nonce for self-echo detection.
    // Two nodes on the same machine share NIC addresses, so IP-based
    // self-echo detection fails. The nonce distinguishes our own packets.
    // It is generated before the first bind because the announce datagram
    // each NIC carries is built from it.
    let instance_nonce: [u8; NONCE_SIZE] = {
        use rand_core::RngCore;
        let mut buf = [0u8; NONCE_SIZE];
        rand_core::OsRng.fill_bytes(&mut buf);
        buf
    };

    let Some(mut nics) =
        wait_for_usable_nics(setup, &instance_nonce, retry_interval, &new_iface_tx).await
    else {
        return Ok(());
    };

    // Per-peer state: keyed by peer's (IPv6 link-local, data_port).
    // Data receive does a two-tier lookup: try exact (ip, port) first,
    // then fall back to ip-only. This handles both:
    // - Same-machine Rust peers (send from data_port → exact match)
    // - Cross-machine Python peers (send from ephemeral port → ip-only)
    // Removal path: peers are removed on timeout in the peer_job_timer branch.
    let mut peers: HashMap<(Ipv6Addr, u16), PeerInfo> = HashMap::new();

    // Deduplication cache for data packets received on multiple NICs
    let mut dedup = DeduplicationCache::new();

    // Timers
    let announce_interval = Duration::from_secs_f64(ANNOUNCE_INTERVAL_SECS);
    let peer_job_interval = Duration::from_secs_f64(PEER_JOB_INTERVAL_SECS);
    let peering_timeout = Duration::from_secs_f64(PEERING_TIMEOUT_SECS);
    let echo_timeout = Duration::from_secs_f64(MCAST_ECHO_TIMEOUT_SECS);
    let reverse_peering_interval = Duration::from_secs_f64(super::reverse_peering_interval_secs());

    let mut announce_timer = tokio::time::interval(announce_interval);
    let mut peer_job_timer = tokio::time::interval(peer_job_interval);

    let mut mcast_buf = [0u8; 64];
    let mut unicast_buf = [0u8; 64];
    let mut data_buf = [0u8; MAX_DATAGRAM_SIZE];
    let mut mcast_poll = 0usize;
    let mut unicast_poll = 0usize;
    let mut data_poll = 0usize;

    loop {
        tokio::select! {
            // Multicast discovery recv
            result = recv_from_any_by(&nics, |n| &n.mcast, &mut mcast_buf, &mut mcast_poll) => {
                let recv = match result {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("AutoInterface: multicast recv error: {}", e);
                        continue;
                    }
                };
                let data = &mcast_buf[..recv.bytes_read];
                let src_addr = *recv.source.ip();
                handle_discovery_packet(
                    data,
                    src_addr,
                    &mut nics[recv.socket_index],
                    &config,
                    &group_tag,
                    &instance_nonce,
                    &mut peers,
                    &peer_count_tx,
                    &next_id,
                    &new_iface_tx,
                );
            }

            // Unicast discovery recv
            result = recv_from_any_by(&nics, |n| &n.unicast, &mut unicast_buf, &mut unicast_poll) => {
                let recv = match result {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("AutoInterface: unicast recv error: {}", e);
                        continue;
                    }
                };
                let data = &unicast_buf[..recv.bytes_read];
                let src_addr = *recv.source.ip();

                handle_discovery_packet(
                    data,
                    src_addr,
                    &mut nics[recv.socket_index],
                    &config,
                    &group_tag,
                    &instance_nonce,
                    &mut peers,
                    &peer_count_tx,
                    &next_id,
                    &new_iface_tx,
                );
            }

            // Data recv + demux
            result = recv_from_any_by(&nics, |n| n.data.as_ref(), &mut data_buf, &mut data_poll) => {
                let recv = match result {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("AutoInterface: data recv error: {}", e);
                        continue;
                    }
                };

                let data = &data_buf[..recv.bytes_read];
                if data.is_empty() {
                    continue;
                }

                // Dedup (same packet from multiple NICs)
                if dedup.is_duplicate(data) {
                    continue;
                }

                // Two-tier peer lookup:
                // 1. Try exact (ip, src_port), works for same-machine Rust peers
                //    that send from their data_port via the NIC data socket.
                // 2. Fall back to ip-only, works for cross-machine Python peers
                //    that send from ephemeral ports.
                let src_ip = *recv.source.ip();
                let src_port = recv.source.port();

                // Resolve the lookup key: exact match first, then ip-only fallback
                let lookup_key = if peers.contains_key(&(src_ip, src_port)) {
                    Some((src_ip, src_port))
                } else {
                    peers.keys().find(|(ip, _)| *ip == src_ip).copied()
                };

                if let Some(peer) = lookup_key.and_then(|k| peers.get_mut(&k)) {
                    peer.last_heard = Instant::now();
                    peer.counters.rx_bytes.fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    match peer.incoming_tx.try_send(IncomingPacket {
                        data: data.to_vec(),
                    }) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            tracing::debug!(
                                "AutoInterface: incoming channel full for {}, dropping packet",
                                src_ip
                            );
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            tracing::warn!(
                                "AutoInterface: incoming channel closed for {}",
                                src_ip
                            );
                        }
                    }
                } else {
                    tracing::debug!(
                        "AutoInterface: data from unknown peer {}, dropping (no discovery yet)",
                        src_ip
                    );
                }
            }

            // Announce timer (send multicast token)
            _ = announce_timer.tick() => {
                for nic in &nics {
                    let dest = SocketAddrV6::new(mcast_addr, config.discovery_port, 0, nic.index);
                    if let Err(e) = nic.mcast.send_to(&nic.discovery_packet, dest).await {
                        tracing::debug!(
                            "AutoInterface: multicast send on {} failed: {}",
                            nic.name,
                            e
                        );
                    }
                }
            }

            // Peer job timer (timeout + reverse peering + carrier)
            _ = peer_job_timer.tick() => {
                let now = Instant::now();

                // Check peer timeouts
                let timed_out: Vec<(Ipv6Addr, u16)> = peers
                    .iter()
                    .filter(|(_, p)| now.duration_since(p.last_heard) > peering_timeout)
                    .map(|(key, _)| *key)
                    .collect();

                for key in &timed_out {
                    if let Some(peer) = peers.remove(key) {
                        tracing::info!(
                            "AutoInterface: peer {}:{} on {} timed out",
                            key.0,
                            key.1,
                            peer.nic_name,
                        );
                        // Dropping incoming_tx triggers the cascade:
                        // event loop detects Disconnected → handle_interface_down → cleanup
                    }
                }
                if !timed_out.is_empty() {
                    let _ = peer_count_tx.send(peers.len());
                }

                // Send reverse peering tokens to peers that haven't been poked recently
                for ((peer_ip, _peer_data_port), peer) in &mut peers {
                    if now.duration_since(peer.last_reverse_peering) > reverse_peering_interval {
                        peer.last_reverse_peering = now;
                        // Token must be verifiable by the peer using OUR source IP
                        let token = match compute_reverse_peering_token(&peer.nic_name, &nics) {
                            Some(t) => t,
                            None => continue,
                        };
                        let pkt = build_discovery_packet(&token, &instance_nonce, config.data_port);
                        let dest = SocketAddrV6::new(
                            *peer_ip,
                            unicast_port,
                            0,
                            peer.scope_id,
                        );
                        // Send from the NIC the peer was discovered on: its
                        // multicast socket is already bound to that interface.
                        if let Some(nic) = nics.iter().find(|n| n.name == peer.nic_name) {
                            if let Err(e) = nic.mcast.send_to(&pkt, dest).await {
                                tracing::debug!(
                                    "AutoInterface: reverse peering send to {} failed: {}",
                                    peer_ip,
                                    e
                                );
                            }
                        }
                    }
                }

                // Check multicast echo timeouts (carrier detection)
                for nic in &mut nics {
                    let echo_timed_out = match nic.last_echo {
                        Some(last) => now.duration_since(last) > echo_timeout,
                        None => continue, // No echo yet — normal at startup
                    };

                    if echo_timed_out && !nic.timed_out {
                        nic.timed_out = true;
                        tracing::warn!(
                            "AutoInterface: multicast echo timeout on {}. Carrier lost.",
                            nic.name
                        );
                    } else if !echo_timed_out && nic.timed_out {
                        nic.timed_out = false;
                        tracing::warn!(
                            "AutoInterface: carrier recovered on {}",
                            nic.name
                        );
                    }
                }
            }

            // Event loop shut down
            _ = new_iface_tx.closed() => {
                tracing::info!("AutoInterface: event loop shut down, exiting");
                break;
            }
        }
    }

    Ok(())
}

/// Process a discovery packet received via multicast or unicast.
///
/// Accepts both 32-byte (Python: token only) and 40-byte (Rust: token + nonce)
/// packets. Token is always at bytes [0..32].
///
/// Self-echo detection:
/// - 40-byte packet with matching nonce: self-echo → carrier detection, discard
/// - 40-byte packet with different nonce: peer Rust node → add peer
/// - 32-byte packet: NEVER self-echo (we only send 40-byte), always from Python → add peer
#[allow(clippy::too_many_arguments)]
fn handle_discovery_packet(
    data: &[u8],
    src_addr: Ipv6Addr,
    nic: &mut Nic,
    config: &AutoInterfaceConfig,
    group_tag: &Option<String>,
    instance_nonce: &[u8; NONCE_SIZE],
    peers: &mut HashMap<(Ipv6Addr, u16), PeerInfo>,
    peer_count_tx: &watch::Sender<usize>,
    next_id: &Arc<AtomicUsize>,
    new_iface_tx: &mpsc::Sender<InterfaceHandle>,
) {
    // Parse token (+ optional nonce)
    let parsed = match parse_discovery_packet(data) {
        Some(p) => p,
        None => {
            tracing::debug!(
                "AutoInterface: malformed discovery packet ({} bytes) from {} on {}: {:02x?}",
                data.len(),
                src_addr,
                nic.name,
                &data[..data.len().min(64)]
            );
            return;
        }
    };

    // Verify token
    let src_str = src_addr.to_string();
    if !verify_discovery_token(&parsed.token, &config.group_id, &src_str) {
        let expected = make_discovery_token(&config.group_id, &src_str);
        tracing::debug!(
            "AutoInterface: invalid discovery token from {} on {} (len={}, token={:02x?}, expected={:02x?}, group={:?}, src_str={:?})",
            src_addr,
            nic.name,
            data.len(),
            &parsed.token[..8],
            &expected[..8],
            String::from_utf8_lossy(&config.group_id),
            src_str
        );
        return;
    }

    // Self-echo detection: only possible for 40-byte packets (which have a nonce).
    // 32-byte packets (Python) are NEVER self-echo because we only send 40-byte.
    if let Some(nonce) = parsed.nonce {
        if nonce == *instance_nonce {
            // Our own 40-byte packet echoed back, carrier detection
            nic.last_echo = Some(Instant::now());
            return;
        }
    }

    // Peer data_port: from discovery packet if present, else config default.
    // Used as destination port when sending TO this peer, and as part of the
    // peer map key for same-machine disambiguation.
    let peer_data_port = parsed.data_port.unwrap_or(config.data_port);
    let peer_key = (src_addr, peer_data_port);

    // Known peer, refresh
    if let Some(peer) = peers.get_mut(&peer_key) {
        peer.last_heard = Instant::now();
        return;
    }

    // New peer, register
    let id = InterfaceId(next_id.fetch_add(1, Ordering::Relaxed));
    let (incoming_tx, incoming_rx) = mpsc::channel(PEER_CHANNEL_BUFFER);
    let (outgoing_tx, outgoing_rx) = mpsc::channel(PEER_CHANNEL_BUFFER);

    let counters = Arc::new(InterfaceCounters::new());

    // Spawn per-peer send task, sends via NIC data socket to peer's
    // (IP, data_port). Using the NIC data socket means our source port =
    // our data_port, which the peer matches against its peer map key.
    let peer_dest = SocketAddrV6::new(src_addr, peer_data_port, 0, nic.index);
    let send_socket = Arc::clone(&nic.data);
    let send_counters = Arc::clone(&counters);
    tokio::spawn(async move {
        peer_send_task(outgoing_rx, send_socket, peer_dest, send_counters).await;
    });

    // Format interface name: IP suffix + port (to distinguish same-IP peers),
    // plus a per-group tag so the same peer discovered in two groups does not
    // collide (empty for the default group).
    let o = src_addr.octets();
    let addr_short = format!("{:02x}{:02x}{:02x}{:02x}", o[12], o[13], o[14], o[15]);
    let base_name = if peer_data_port == config.data_port {
        format!("auto/{}/{}", nic.name, addr_short)
    } else {
        format!("auto/{}/{}:{}", nic.name, addr_short, peer_data_port)
    };
    let iface_name = match group_tag {
        Some(tag) => format!("{}#{}", base_name, tag),
        None => base_name,
    };

    let handle = InterfaceHandle {
        info: InterfaceInfo {
            id,
            name: iface_name.clone(),
            hw_mtu: Some(AUTO_HW_MTU),
            is_local_client: false,
            bitrate: None,
            announce_cap_bitrate: None,
            tx_jitter_max_ms: None,
            ifac: None,
            mode: leviculum_core::traits::InterfaceMode::default(),
            kind: leviculum_core::traits::InterfaceKind::Auto,
            ingress_control: None,
        },
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters: Arc::clone(&counters),
        credit: None,
        // AutoInterface peers materialise per discovery; the UDP
        // socket pair already exists by the time this handle is
        // constructed, so the interface is ready immediately.
        ready: crate::interfaces::ReadySignal::ready_immediate(),
    };

    let now = Instant::now();
    peers.insert(
        peer_key,
        PeerInfo {
            nic_name: nic.name.clone(),
            scope_id: nic.index,
            last_heard: now,
            last_reverse_peering: now,
            incoming_tx,
            counters,
        },
    );
    let _ = peer_count_tx.send(peers.len());

    match new_iface_tx.try_send(handle) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!(
                "AutoInterface: new-interface channel full, cannot register peer {}:{}",
                src_addr,
                peer_data_port,
            );
            peers.remove(&peer_key);
            let _ = peer_count_tx.send(peers.len());
            return;
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!("AutoInterface: event loop gone, cannot register peer");
            peers.remove(&peer_key);
            let _ = peer_count_tx.send(peers.len());
            return;
        }
    }

    tracing::info!(
        "AutoInterface: new peer {}:{} on {} (id={}, name={})",
        src_addr,
        peer_data_port,
        nic.name,
        id,
        iface_name
    );
}

/// Per-peer send task: reads outgoing packets from the event loop and
/// sends them to the peer's address via the shared outbound socket.
///
/// Exits when `outgoing_rx` is closed (event loop dropped the sender,
/// which happens when the InterfaceHandle is removed from the registry).
async fn peer_send_task(
    mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
    socket: Arc<UdpSocket>,
    peer_addr: SocketAddrV6,
    counters: Arc<InterfaceCounters>,
) {
    while let Some(pkt) = outgoing_rx.recv().await {
        match socket.send_to(&pkt.data, peer_addr).await {
            Ok(n) => {
                counters
                    .tx_bytes
                    .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            }
            Err(e) => {
                tracing::debug!("AutoInterface: send to {} failed: {}", peer_addr.ip(), e);
                // Don't break, send errors are transient for UDP
            }
        }
    }
    tracing::debug!("AutoInterface: send task for {} exiting", peer_addr.ip());
}

#[cfg(test)]
mod tests {
    use super::*;
    use socket2::{Domain, Protocol, SockAddr, Type};

    /// The nonce every test NIC bakes into its announce datagram. Any value
    /// works; what matters is that it is the one the self-echo check compares
    /// against, so `DiscoveryTestCtx` uses the same one.
    const TEST_NONCE: [u8; NONCE_SIZE] = [0x42u8; NONCE_SIZE];

    /// Three ephemeral sockets standing in for one NIC's socket set. Past
    /// binding, nothing in the orchestrator cares which interface they are on.
    fn fake_nic_sockets() -> NicSockets {
        NicSockets {
            mcast: bind_test_socket(),
            unicast: bind_test_socket(),
            data: bind_test_socket(),
        }
    }

    /// One `Nic` on ephemeral sockets, with its token and announce datagram
    /// derived exactly as a real bind derives them.
    fn fake_nic(name: &str, link_local: &str, index: u32, group_id: &[u8]) -> Nic {
        Nic::new(
            &AdoptedNic {
                name: name.into(),
                link_local: link_local.parse().unwrap(),
                index,
            },
            fake_nic_sockets(),
            group_id,
            &TEST_NONCE,
            AutoInterfaceConfig::default().data_port,
        )
    }

    fn adopted(name: &str, link_local: &str, index: u32) -> AdoptedNic {
        AdoptedNic {
            name: name.into(),
            link_local: link_local.parse().unwrap(),
            index,
        }
    }

    /// A NIC whose bind fails must not shift the NICs that come after it.
    ///
    /// That property used to need `build_active_nics_and_tokens`, which
    /// skipped the same index in six parallel containers. The assertion is
    /// unchanged: entry 0 is eth1, and the discovery material entry 0 carries
    /// is eth1's — only now a failed NIC has no entry at all rather than
    /// being skipped consistently everywhere.
    #[tokio::test]
    async fn test_active_nics_skip_failed_binds() {
        let nics = vec![
            adopted("eth0", "fe80::1", 1),
            adopted("eth1", "fe80::2", 2),
            adopted("eth2", "fe80::3", 3),
        ];
        // eth0 failed to bind, eth1 and eth2 succeeded
        let bound = bind_nics_with(&nics, b"reticulum", &TEST_NONCE, 42671, |nic| {
            (nic.name != "eth0").then(fake_nic_sockets)
        });

        // nics[0] must be eth1, NOT eth0
        assert_eq!(bound.len(), 2);
        assert_eq!(bound[0].name, "eth1");
        assert_eq!(bound[1].name, "eth2");

        // The token at index 0 must be for eth1's address (fe80::2)
        let expected = make_discovery_token(b"reticulum", "fe80::2");
        assert_eq!(
            bound[0].token, expected,
            "token[0] must match eth1, not eth0"
        );
        assert_eq!(
            &bound[0].discovery_packet[..32],
            &expected[..],
            "the announce datagram must carry the same NIC's token"
        );
    }

    #[tokio::test]
    async fn test_active_nics_all_succeed() {
        let nics = vec![adopted("eth0", "fe80::1", 1), adopted("eth1", "fe80::2", 2)];

        let bound = bind_nics_with(&nics, b"reticulum", &TEST_NONCE, 42671, |_| {
            Some(fake_nic_sockets())
        });

        assert_eq!(bound.len(), 2);
        assert_eq!(bound[0].name, "eth0");
        assert_eq!(bound[1].name, "eth1");
        assert_eq!(
            bound[1].token,
            make_discovery_token(b"reticulum", "fe80::2")
        );
    }

    #[tokio::test]
    async fn test_reverse_peering_token_verifiable_by_peer() {
        let group_id = b"reticulum";
        let nics = vec![fake_nic("eth0", "fe80::1", 1, group_id)];
        // Peer's address is different from ours
        let _peer_addr: Ipv6Addr = "fe80::99".parse().unwrap();

        let token = compute_reverse_peering_token("eth0", &nics).expect("should find NIC");

        // Peer receives this token and verifies against our source IP (fe80::1)
        assert!(
            verify_discovery_token(&token, group_id, "fe80::1"),
            "token must verify against sender's source IP, not peer's address"
        );
        // Must NOT verify against the peer's address
        assert!(
            !verify_discovery_token(&token, group_id, "fe80::99"),
            "token must NOT verify against peer's own address"
        );
    }

    #[tokio::test]
    async fn test_reverse_peering_token_unknown_nic() {
        let nics = vec![fake_nic("eth0", "fe80::1", 1, b"reticulum")];

        let result = compute_reverse_peering_token("wlan0", &nics);
        assert!(result.is_none(), "unknown NIC should return None");
    }

    #[tokio::test]
    async fn test_active_nics_all_fail() {
        let nics = vec![adopted("eth0", "fe80::1", 1)];

        let bound = bind_nics_with(&nics, b"reticulum", &TEST_NONCE, 42671, |_| None);

        assert!(bound.is_empty());
    }

    #[test]
    fn test_peer_info_fields() {
        // Basic construction test, verifies the struct layout compiles
        let (tx, _rx) = mpsc::channel(1);
        let _peer = PeerInfo {
            nic_name: "eth0".to_string(),
            scope_id: 2,
            last_heard: Instant::now(),
            last_reverse_peering: Instant::now(),
            incoming_tx: tx,
            counters: Arc::new(InterfaceCounters::new()),
        };
    }

    /// Bind a UDP socket on [::]:0 for test purposes
    fn bind_test_socket() -> UdpSocket {
        let socket = socket2::Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        socket.set_nonblocking(true).unwrap();
        let bind_addr = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0);
        socket.bind(&SockAddr::from(bind_addr)).unwrap();
        UdpSocket::from_std(socket.into()).unwrap()
    }

    #[tokio::test]
    async fn test_peer_send_task_exits_on_channel_close() {
        let (outgoing_tx, outgoing_rx) = mpsc::channel(8);
        let socket = Arc::new(bind_test_socket());
        // Use a dummy address, we won't actually receive
        let peer_addr = SocketAddrV6::new("::1".parse().unwrap(), 9999, 0, 0);

        let counters = Arc::new(InterfaceCounters::new());
        let handle = tokio::spawn(async move {
            peer_send_task(outgoing_rx, socket, peer_addr, counters).await;
        });

        // Drop the sender, task should exit
        drop(outgoing_tx);
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("peer_send_task should exit within 2s")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn test_peer_send_task_forwards_data() {
        let (outgoing_tx, outgoing_rx) = mpsc::channel(8);
        let socket = Arc::new(bind_test_socket());

        // Bind a receiver socket to verify the send arrives
        let recv_socket = bind_test_socket();
        let recv_addr = recv_socket.local_addr().unwrap();
        let recv_v6 = match recv_addr {
            std::net::SocketAddr::V6(v6) => v6,
            _ => panic!("expected v6"),
        };

        // Rewrite to use loopback with correct port
        let peer_addr = SocketAddrV6::new("::1".parse().unwrap(), recv_v6.port(), 0, 0);
        let counters = Arc::new(InterfaceCounters::new());

        tokio::spawn(async move {
            peer_send_task(outgoing_rx, socket, peer_addr, counters).await;
        });

        // Send a packet
        outgoing_tx
            .send(OutgoingPacket {
                peer: None,
                data: b"test data".to_vec(),
                high_priority: false,
            })
            .await
            .unwrap();

        let mut buf = [0u8; 64];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), recv_socket.recv_from(&mut buf))
            .await
            .expect("timeout")
            .expect("recv error");
        assert_eq!(&buf[..n], b"test data");
    }

    /// Helper to set up a standard test context for handle_discovery_packet tests
    struct DiscoveryTestCtx {
        config: AutoInterfaceConfig,
        /// The NIC the packet arrived on. It carries its own sockets and its
        /// own echo state now, so the test context has neither a separate
        /// data socket nor a `nic_states` map.
        nic: Nic,
        peers: HashMap<(Ipv6Addr, u16), PeerInfo>,
        peer_count_tx: watch::Sender<usize>,
        _peer_count_rx: watch::Receiver<usize>,
        next_id: Arc<AtomicUsize>,
        new_iface_tx: mpsc::Sender<InterfaceHandle>,
        new_iface_rx: mpsc::Receiver<InterfaceHandle>,
        our_nonce: [u8; NONCE_SIZE],
        group_tag: Option<String>,
    }

    impl DiscoveryTestCtx {
        fn new() -> Self {
            let (peer_count_tx, _peer_count_rx) = watch::channel(0usize);
            let (new_iface_tx, new_iface_rx) = mpsc::channel(8);
            let config = AutoInterfaceConfig::default();
            let group_tag = group_name_tag(&config.group_id);
            let nic = fake_nic("eth0", "fe80::1", 1, &config.group_id);
            Self {
                config,
                nic,
                peers: HashMap::new(),
                peer_count_tx,
                _peer_count_rx,
                next_id: Arc::new(AtomicUsize::new(100)),
                new_iface_tx,
                new_iface_rx,
                our_nonce: TEST_NONCE,
                group_tag,
            }
        }

        fn call(&mut self, data: &[u8], src_addr: Ipv6Addr) {
            handle_discovery_packet(
                data,
                src_addr,
                &mut self.nic,
                &self.config,
                &self.group_tag,
                &self.our_nonce,
                &mut self.peers,
                &self.peer_count_tx,
                &self.next_id,
                &self.new_iface_tx,
            );
        }
    }

    #[tokio::test]
    async fn test_self_echo_not_added_as_peer() {
        let mut ctx = DiscoveryTestCtx::new();
        ctx.nic.timed_out = true;

        // Create a valid 42-byte discovery packet with OUR nonce
        let token = make_discovery_token(&ctx.config.group_id, "fe80::1");
        let pkt = build_discovery_packet(&token, &ctx.our_nonce, ctx.config.data_port);

        ctx.call(&pkt, "fe80::1".parse().unwrap());

        // Should NOT create a peer (self-echo: nonce matches)
        assert!(ctx.peers.is_empty(), "self-echo should not create a peer");
        // Should update echo timestamp
        assert!(
            ctx.nic.last_echo.is_some(),
            "self-echo should update echo timestamp"
        );
    }

    #[tokio::test]
    async fn test_new_peer_registered() {
        let mut ctx = DiscoveryTestCtx::new();

        // Create a discovery packet from a different instance (different nonce)
        let peer_nonce = [0x99u8; NONCE_SIZE];
        let peer_addr: Ipv6Addr = "fe80::2".parse().unwrap();
        let token = make_discovery_token(&ctx.config.group_id, &peer_addr.to_string());
        let pkt = build_discovery_packet(&token, &peer_nonce, ctx.config.data_port);

        ctx.call(&pkt, peer_addr);

        assert_eq!(ctx.peers.len(), 1, "should have one peer");
        let peer_key = (peer_addr, ctx.config.data_port);
        assert!(ctx.peers.contains_key(&peer_key));

        // Should have sent an InterfaceHandle to the event loop
        let handle = ctx.new_iface_rx.try_recv().expect("should receive handle");
        assert_eq!(handle.info.id, InterfaceId(100));
        assert!(handle.info.name.starts_with("auto/eth0/"));
        assert_eq!(handle.info.hw_mtu, Some(AUTO_HW_MTU));
    }

    #[tokio::test]
    async fn test_known_peer_refreshed() {
        let mut ctx = DiscoveryTestCtx::new();

        let peer_nonce = [0x99u8; NONCE_SIZE];
        let peer_addr: Ipv6Addr = "fe80::2".parse().unwrap();
        let token = make_discovery_token(&ctx.config.group_id, &peer_addr.to_string());
        let pkt = build_discovery_packet(&token, &peer_nonce, ctx.config.data_port);
        let peer_key = (peer_addr, ctx.config.data_port);

        // First discovery
        ctx.call(&pkt, peer_addr);
        let first_heard = ctx.peers[&peer_key].last_heard;

        // Brief delay
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Second discovery, should refresh, not add new
        ctx.call(&pkt, peer_addr);

        assert_eq!(ctx.peers.len(), 1, "should still have one peer");
        assert!(
            ctx.peers[&peer_key].last_heard > first_heard,
            "last_heard should be refreshed"
        );
    }

    #[tokio::test]
    async fn test_invalid_token_rejected() {
        let mut ctx = DiscoveryTestCtx::new();

        // Send a bogus 40-byte packet (correct size but wrong token)
        let bogus = [0u8; DISCOVERY_PACKET_SIZE];
        ctx.call(&bogus, "fe80::2".parse().unwrap());

        assert!(
            ctx.peers.is_empty(),
            "invalid token should not create a peer"
        );
    }

    #[tokio::test]
    async fn test_same_ip_different_nonce_not_self_echo() {
        // Two nodes on the same machine: same IP, different nonces
        let mut ctx = DiscoveryTestCtx::new();
        ctx.next_id = Arc::new(AtomicUsize::new(0));

        let peer_nonce = [0x99u8; NONCE_SIZE];

        // Source IP is our own (fe80::1), but nonce differs → NOT self-echo
        let token = make_discovery_token(&ctx.config.group_id, "fe80::1");
        let pkt = build_discovery_packet(&token, &peer_nonce, ctx.config.data_port);

        ctx.call(&pkt, "fe80::1".parse().unwrap());

        assert_eq!(
            ctx.peers.len(),
            1,
            "same IP but different nonce should create a peer"
        );
        let _handle = ctx
            .new_iface_rx
            .try_recv()
            .expect("should register interface");
    }

    #[tokio::test]
    async fn test_32_byte_python_packet_creates_peer() {
        // 32-byte Python packet (token only, no nonce) should always create a peer
        // and never be treated as self-echo
        let mut ctx = DiscoveryTestCtx::new();
        ctx.next_id = Arc::new(AtomicUsize::new(0));

        let peer_addr: Ipv6Addr = "fe80::3".parse().unwrap();
        let token = make_discovery_token(&ctx.config.group_id, &peer_addr.to_string());

        // Send just the 32-byte token (Python format)
        ctx.call(&token, peer_addr);

        assert_eq!(
            ctx.peers.len(),
            1,
            "32-byte Python packet should create a peer"
        );
        let _handle = ctx
            .new_iface_rx
            .try_recv()
            .expect("should register interface");
    }

    #[tokio::test]
    async fn test_peer_name_tagged_in_non_default_group() {
        // Codeberg #7: a peer discovered in a non-default group carries the
        // group tag in its interface name so it does not collide with the same
        // peer discovered in another group.
        let mut ctx = DiscoveryTestCtx::new();
        ctx.next_id = Arc::new(AtomicUsize::new(0));
        ctx.config.group_id = b"groupA".to_vec();
        ctx.group_tag = group_name_tag(&ctx.config.group_id);
        let expected_tag = ctx.group_tag.clone().expect("non-default group is tagged");

        let peer_nonce = [0x99u8; NONCE_SIZE];
        let peer_addr: Ipv6Addr = "fe80::2".parse().unwrap();
        let token = make_discovery_token(&ctx.config.group_id, &peer_addr.to_string());
        let pkt = build_discovery_packet(&token, &peer_nonce, ctx.config.data_port);

        ctx.call(&pkt, peer_addr);

        let handle = ctx.new_iface_rx.try_recv().expect("should register handle");
        assert!(
            handle.info.name.ends_with(&format!("#{}", expected_tag)),
            "non-default-group peer name should carry the group tag, got {}",
            handle.info.name
        );
    }

    #[tokio::test]
    async fn test_peer_name_untagged_in_default_group() {
        // Default group keeps the historical untagged name.
        let mut ctx = DiscoveryTestCtx::new();
        ctx.next_id = Arc::new(AtomicUsize::new(0));

        let peer_nonce = [0x99u8; NONCE_SIZE];
        let peer_addr: Ipv6Addr = "fe80::2".parse().unwrap();
        let token = make_discovery_token(&ctx.config.group_id, &peer_addr.to_string());
        let pkt = build_discovery_packet(&token, &peer_nonce, ctx.config.data_port);

        ctx.call(&pkt, peer_addr);

        let handle = ctx.new_iface_rx.try_recv().expect("should register handle");
        assert!(
            !handle.info.name.contains('#'),
            "default-group peer name must not carry a group tag, got {}",
            handle.info.name
        );
        assert!(handle.info.name.starts_with("auto/eth0/"));
    }

    /// Two AutoInterface sections with distinct group_ids AND distinct ports
    /// must spawn without an `AddrInUse` panic. On a CI host without a suitable
    /// link-local NIC each orchestrator sits in the NIC wait loop; on a host
    /// with one they bind real (distinct) sockets. Either way the spawn path
    /// must not panic and both peer-count channels start at 0.
    ///
    /// Real cross-group multicast isolation (a peer announced in group A must
    /// not appear in group B) needs an actual multi-NIC LAN and is not
    /// reproducible in CI; it is validated at the address layer by
    /// `test_derive_multicast_address_group_isolation`.
    #[tokio::test]
    async fn test_two_sections_distinct_ports_spawn_without_conflict() {
        let next_id = Arc::new(AtomicUsize::new(0));
        let (new_iface_tx, _new_iface_rx) = mpsc::channel(16);

        let cfg_a = AutoInterfaceConfig {
            group_id: b"groupA".to_vec(),
            discovery_port: 39716,
            data_port: 42671,
            ..AutoInterfaceConfig::default()
        };
        let cfg_b = AutoInterfaceConfig {
            group_id: b"groupB".to_vec(),
            discovery_port: 39816,
            data_port: 42771,
            ..AutoInterfaceConfig::default()
        };

        let rx_a = spawn_auto_interface(next_id.clone(), new_iface_tx.clone(), cfg_a);
        let rx_b = spawn_auto_interface(next_id.clone(), new_iface_tx.clone(), cfg_b);

        // Give the orchestrators a moment to bind or exit cleanly.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(*rx_a.borrow(), 0);
        assert_eq!(*rx_b.borrow(), 0);
    }

    #[tokio::test]
    async fn test_32_byte_packet_from_own_ip_not_self_echo() {
        // Even if the 32-byte packet comes from our own IP, it cannot be
        // self-echo because we only send 40-byte packets
        let mut ctx = DiscoveryTestCtx::new();
        ctx.next_id = Arc::new(AtomicUsize::new(0));

        let token = make_discovery_token(&ctx.config.group_id, "fe80::1");

        // Send 32-byte token from our own link-local address
        ctx.call(&token, "fe80::1".parse().unwrap());

        assert_eq!(
            ctx.peers.len(),
            1,
            "32-byte packet from own IP should still create peer (never self-echo)"
        );
    }

    /// One NIC's worth of ephemeral sockets, standing in for a NIC that has
    /// just appeared, plus the port its multicast socket listens on so the
    /// test can announce to it.
    ///
    /// No real NIC is involved: past enumeration and binding, the discovery
    /// loop only needs three bound sockets, which is exactly why the
    /// bring-up step is injected.
    fn fake_bound_sockets() -> (NicSockets, u16) {
        let sockets = fake_nic_sockets();
        let port = match sockets.mcast.local_addr().unwrap() {
            std::net::SocketAddr::V6(v6) => v6.port(),
            _ => panic!("expected v6"),
        };
        (sockets, port)
    }

    /// The minimal test for the ordinary boot of a laptop or a small board:
    /// the daemon starts before the network does, so the first rounds find no
    /// suitable NIC. When one appears the interface has to come up by itself,
    /// without a restart. Before the fix the orchestrator returned on the
    /// first empty enumeration and the node stayed deaf on the LAN for the
    /// rest of its life.
    #[tokio::test]
    async fn test_interface_comes_up_when_a_nic_appears_late() {
        let config = AutoInterfaceConfig::default();
        let mcast_addr = derive_multicast_address(
            &config.group_id,
            &config.discovery_scope,
            config.multicast_address_type,
        )
        .unwrap();

        let (new_iface_tx, mut new_iface_rx) = mpsc::channel(8);
        let (peer_count_tx, peer_count_rx) = watch::channel(0usize);

        let (sockets, mcast_port) = fake_bound_sockets();
        let mut sockets = Some(sockets);
        let group_id = config.group_id.clone();
        let data_port = config.data_port;
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_seen = Arc::clone(&attempts);
        // The orchestrator hands its own instance nonce to the bring-up step,
        // so the NIC that appears announces with the nonce the loop will
        // recognise as its own.
        let setup = move |attempt: u64, nonce: &[u8; NONCE_SIZE]| {
            attempts_seen.store(attempt as usize, Ordering::Relaxed);
            // No suitable NIC on the first two rounds, then one appears.
            if attempt < 3 {
                None
            } else {
                sockets.take().map(|s| {
                    vec![Nic::new(
                        &adopted("late0", "fe80::1", 0),
                        s,
                        &group_id,
                        nonce,
                        data_port,
                    )]
                })
            }
        };

        let run_config = config.clone();
        let task = tokio::spawn(run_auto_interface_with(
            run_config,
            Arc::new(AtomicUsize::new(7)),
            new_iface_tx,
            peer_count_tx,
            mcast_addr,
            setup,
            Duration::from_millis(20),
        ));

        // A peer announces itself on the NIC that just appeared. The datagram
        // is queued by the kernel, so it does not matter whether the
        // orchestrator is already reading when it is sent.
        let peer_socket = bind_test_socket();
        let token = make_discovery_token(&config.group_id, "::1");
        let pkt = build_discovery_packet(&token, &[0x99u8; NONCE_SIZE], config.data_port);
        let dest = SocketAddrV6::new("::1".parse().unwrap(), mcast_port, 0, 0);
        peer_socket.send_to(&pkt, dest).await.unwrap();

        let handle = tokio::time::timeout(Duration::from_secs(5), new_iface_rx.recv())
            .await
            .expect("interface must come up after the NIC appears, without a restart")
            .expect("orchestrator must register the peer");

        assert!(
            handle.info.name.starts_with("auto/late0/"),
            "peer must be registered on the late NIC, got {}",
            handle.info.name
        );
        assert!(
            attempts.load(Ordering::Relaxed) >= 3,
            "orchestrator must re-check after an empty enumeration, checks={}",
            attempts.load(Ordering::Relaxed)
        );
        assert_eq!(*peer_count_rx.borrow(), 1, "peer count must be mirrored");

        task.abort();
    }

    /// A host that never gets a suitable NIC must keep looking rather than
    /// end the task, and only a shutdown of the event loop ends the wait.
    #[tokio::test]
    async fn test_no_nic_keeps_waiting_and_exits_only_on_shutdown() {
        let config = AutoInterfaceConfig::default();
        let mcast_addr = derive_multicast_address(
            &config.group_id,
            &config.discovery_scope,
            config.multicast_address_type,
        )
        .unwrap();

        let (new_iface_tx, new_iface_rx) = mpsc::channel(8);
        let (peer_count_tx, _peer_count_rx) = watch::channel(0usize);

        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_seen = Arc::clone(&attempts);
        let setup = move |attempt: u64, _nonce: &[u8; NONCE_SIZE]| {
            attempts_seen.store(attempt as usize, Ordering::Relaxed);
            None
        };

        let task = tokio::spawn(run_auto_interface_with(
            config,
            Arc::new(AtomicUsize::new(0)),
            new_iface_tx,
            peer_count_tx,
            mcast_addr,
            setup,
            Duration::from_millis(10),
        ));

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !task.is_finished(),
            "an empty NIC list is a waiting state, not an exit"
        );
        assert!(
            attempts.load(Ordering::Relaxed) >= 3,
            "orchestrator must re-check on the retry interval, checks={}",
            attempts.load(Ordering::Relaxed)
        );

        // The event loop going away is the one reason to give up.
        drop(new_iface_rx);
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("orchestrator must stop waiting once the event loop is gone")
            .expect("task should not panic");
        assert!(result.is_ok(), "shutdown while waiting is not an error");
    }

    /// The production bring-up step reports "nothing usable this round"
    /// instead of failing, on a host where the device whitelist matches no
    /// interface. Host-independent: the whitelist excludes everything.
    #[test]
    fn test_try_bind_nics_none_when_no_nic_matches() {
        let config = AutoInterfaceConfig {
            allowed_devices: Some("lev-no-such-nic".to_string()),
            ..AutoInterfaceConfig::default()
        };
        let mcast_addr = derive_multicast_address(
            &config.group_id,
            &config.discovery_scope,
            config.multicast_address_type,
        )
        .unwrap();

        let result = try_bind_nics(
            &config,
            &mcast_addr,
            unicast_discovery_port(config.discovery_port),
            &TEST_NONCE,
            1,
        );
        assert!(
            result.is_none(),
            "no matching NIC must be a waiting state, not a bound interface"
        );
    }
}
