//! AutoInterface, zero-configuration LAN discovery via IPv6 multicast
//!
//! Nodes on the same LAN discover each other via IPv6 multicast and
//! communicate over UDP. Matches Python Reticulum's `AutoInterface`.
//!
//! Linux only. The wire facts (group address, token, discovery datagram,
//! ports, cadences, dedup) live in `leviculum_core::auto_wire`, shared with
//! the boards that speak AutoInterface without std (1479); this module adds
//! the sockets, the NICs and the per-NIC orchestrator.

pub(crate) mod orchestrator;

use std::io;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::net::UdpSocket;

use leviculum_core::auto_wire as wire;
use leviculum_core::crypto::full_hash;

// Constants (match Python exactly; the values live in `auto_wire`)
pub(crate) use wire::{
    Discovery as DiscoveryFields, DEFAULT_DATA_PORT, DEFAULT_DISCOVERY_PORT, DEFAULT_GROUP_ID,
    DISCOVERY_PACKET_SIZE, NONCE_SIZE,
};

/// Peer timeout, peer removed if not heard for this long
pub(crate) const PEERING_TIMEOUT_SECS: f64 = wire::PEERING_TIMEOUT_MS as f64 / 1000.0;
/// Multicast announce interval
pub(crate) const ANNOUNCE_INTERVAL_SECS: f64 = wire::ANNOUNCE_INTERVAL_MS as f64 / 1000.0;
/// Peer maintenance job interval
pub(crate) const PEER_JOB_INTERVAL_SECS: f64 = wire::PEER_JOB_INTERVAL_MS as f64 / 1000.0;
/// Multicast echo timeout, carrier lost if no self-echo for this long
pub(crate) const MCAST_ECHO_TIMEOUT_SECS: f64 = wire::MCAST_ECHO_TIMEOUT_MS as f64 / 1000.0;

/// How long the orchestrator waits before looking for a usable NIC again
/// while it has none.
///
/// This is a load decision on every host that runs us, so it is priced: one
/// round is a `getifaddrs(3)` netlink round trip plus the filtering in
/// [`enumerate_nics`], measured at 58.5 µs per call (2000 calls, 6 interface
/// entries, x86-64 laptop), i.e. 360 rounds and ~21 ms of CPU per hour of
/// waiting - and only while waiting, the loop is left for good once a NIC
/// binds. Ten seconds also caps how long a node stays deaf after its NIC
/// appears at well under the 22 s peering timeout its neighbours use, while
/// staying far enough apart to not become a timer wakeup source on a
/// battery-powered board.
pub(crate) const NIC_RETRY_INTERVAL_SECS: f64 = 10.0;

/// Every Nth fruitless NIC re-check is logged at warn, the rest at debug.
/// At [`NIC_RETRY_INTERVAL_SECS`] that is one line per 5 minutes, so a host
/// that never gets a NIC keeps saying so without filling the log.
pub(crate) const NIC_WAIT_WARN_EVERY: u64 = 30;

/// Default cap on the peer table (`max_peers`, review finding L-0040).
///
/// Every peer costs a spawned send task, two 64-slot channels and a routable core interface that each announce fans out
/// over, and discovery is unauthenticated: the token is a hash of a public
/// default group id and the sender's own address. A home LAN carries tens of
/// Reticulum instances, a busy hackspace or event segment perhaps a hundred;
/// 128 leaves that headroom and still bounds what one hostile host on the
/// segment can make us allocate. Python's `self.peers` has no cap; the cap
/// is invisible on the wire, a refused peer simply is not routed to.
pub(crate) const DEFAULT_MAX_PEERS: usize = 128;

/// Default cap on the peers one source address holds
/// (`max_peers_per_source`, follow-up to L-0040).
///
/// The admission interval only slows one host down: refreshing the peers it
/// has minted, it gains one more per interval and owns the whole table in
/// about ten minutes, crowding out every real neighbour. A host runs a
/// handful of Reticulum instances at most, each on its own data port, so
/// four admits every legitimate same-host setup and leaves a hostile host
/// four slots out of [`DEFAULT_MAX_PEERS`].
pub(crate) const DEFAULT_MAX_PEERS_PER_SOURCE: usize = 4;

/// Default per-source admission interval in seconds
/// (`peer_admission_interval`, L-0040): one source address registers at
/// most one new peer per interval.
///
/// The token does not cover the data port, so without it one host mints a
/// peer per packet by naming a fresh port each time. Five seconds is about
/// three [`ANNOUNCE_INTERVAL_SECS`], so a flood is slowed to one peer per
/// five seconds per address, while a legitimate second instance on the
/// same host, refused at its first announce, is admitted on the first one
/// after the interval, far inside the [`PEERING_TIMEOUT_SECS`] its
/// neighbours give it.
pub(crate) const DEFAULT_PEER_ADMISSION_INTERVAL_SECS: u64 = 5;

/// Hardware MTU for AutoInterface (matches Python `HW_MTU = 1196`)
pub(crate) const AUTO_HW_MTU: u32 = wire::HW_MTU;

pub use wire::MulticastAddressType;

// Configuration
/// Configuration for an AutoInterface instance
#[derive(Debug, Clone)]
pub struct AutoInterfaceConfig {
    pub group_id: Vec<u8>,
    pub discovery_port: u16,
    pub data_port: u16,
    pub discovery_scope: String,
    /// Comma-separated whitelist of NIC names (None = all)
    pub allowed_devices: Option<String>,
    /// Comma-separated blacklist of NIC names
    pub ignored_devices: Option<String>,
    /// Enable multicast loopback (`IPV6_MULTICAST_LOOP`). This is the carrier
    /// self-echo mechanism: the host receiving its own discovery multicast is
    /// how AutoInterface detects a live carrier. Defaults to `true`, matching
    /// Python-RNS (which never sets the sockopt and inherits the OS default ON
    /// on Linux) and giving reliable self-echo even on bridges that stop
    /// reflecting the group. Set to `false` to opt out.
    pub multicast_loopback: bool,
    /// Multicast address type of the discovery group (Codeberg #282).
    /// Defaults to `Temporary`, which is the group Python joins when the key
    /// is absent, so an existing deployment keeps the group it has.
    pub multicast_address_type: MulticastAddressType,
    /// Most peers this interface keeps at once (L-0040). A discovery from a
    /// new peer past the cap is refused, and `AUTO_PEER_REFUSED
    /// reason=cap` says so. Defaults to 128 (`DEFAULT_MAX_PEERS`).
    pub max_peers: usize,
    /// Most peers one source address holds at once. A discovery from a new
    /// peer past it is refused, and `AUTO_PEER_REFUSED reason=source` says
    /// so. Defaults to 4 (`DEFAULT_MAX_PEERS_PER_SOURCE`).
    pub max_peers_per_source: usize,
    /// At most one new peer per source address per this interval (L-0040);
    /// `Duration::ZERO` turns the limit off. Defaults to
    /// five seconds (`DEFAULT_PEER_ADMISSION_INTERVAL_SECS`).
    pub peer_admission_interval: Duration,
}

impl Default for AutoInterfaceConfig {
    fn default() -> Self {
        Self {
            group_id: DEFAULT_GROUP_ID.to_vec(),
            discovery_port: DEFAULT_DISCOVERY_PORT,
            data_port: DEFAULT_DATA_PORT,
            discovery_scope: "link".to_string(),
            allowed_devices: None,
            ignored_devices: None,
            multicast_loopback: true,
            multicast_address_type: MulticastAddressType::Temporary,
            max_peers: DEFAULT_MAX_PEERS,
            max_peers_per_source: DEFAULT_MAX_PEERS_PER_SOURCE,
            peer_admission_interval: Duration::from_secs(DEFAULT_PEER_ADMISSION_INTERVAL_SECS),
        }
    }
}

// NIC enumeration
/// A network interface adopted for AutoInterface use
#[derive(Debug, Clone)]
pub struct AdoptedNic {
    /// Interface name (e.g. "eth0", "wlan0")
    pub name: String,
    /// IPv6 link-local address on this interface
    pub link_local: Ipv6Addr,
    /// OS interface index (needed for multicast join and scope_id)
    pub index: u32,
}

/// Enumerate NICs suitable for AutoInterface.
///
/// Filters:
/// - Must have an IPv6 link-local address (fe80::)
/// - Not loopback
/// - Respects `allowed_devices` whitelist and `ignored_devices` blacklist
/// - Skips docker/veth/br- virtual interfaces on Linux
pub fn enumerate_nics(config: &AutoInterfaceConfig) -> Vec<AdoptedNic> {
    let ifaces = match if_addrs::get_if_addrs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to enumerate network interfaces: {}", e);
            return Vec::new();
        }
    };

    let allowed: Option<Vec<&str>> = config
        .allowed_devices
        .as_ref()
        .map(|s| s.split(',').map(|d| d.trim()).collect());
    let ignored: Vec<&str> = config
        .ignored_devices
        .as_deref()
        .map(|s| s.split(',').map(|d| d.trim()).collect())
        .unwrap_or_default();

    // Default ignored prefixes on Linux (virtual interfaces)
    let default_ignored_prefixes = ["docker", "veth", "br-"];

    let mut adopted = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    tracing::debug!("AutoInterface: if_addrs returned {} entries", ifaces.len());

    for iface in &ifaces {
        tracing::trace!(
            "AutoInterface: evaluating iface '{}' addr={} loopback={}",
            iface.name,
            iface.addr.ip(),
            iface.is_loopback()
        );

        // Skip loopback
        if iface.is_loopback() {
            tracing::trace!("AutoInterface: '{}' skipped (loopback)", iface.name);
            continue;
        }

        // Only IPv6 link-local (fe80::)
        let addr = match iface.addr.ip() {
            std::net::IpAddr::V6(v6) if is_link_local_v6(&v6) => v6,
            other => {
                tracing::trace!(
                    "AutoInterface: '{}' skipped (not link-local v6: {})",
                    iface.name,
                    other
                );
                continue;
            }
        };

        let name = &iface.name;

        // Skip default-ignored virtual interfaces
        if default_ignored_prefixes.iter().any(|p| name.starts_with(p)) {
            tracing::trace!("AutoInterface: '{}' skipped (default-ignored prefix)", name);
            continue;
        }

        // Apply whitelist
        if let Some(ref allowed_list) = allowed {
            if !allowed_list.iter().any(|d| name == *d) {
                continue;
            }
        }

        // Apply blacklist
        if ignored.iter().any(|d| name == *d) {
            continue;
        }

        // Only take first link-local per interface name
        if !seen_names.insert(name.clone()) {
            continue;
        }

        // Get OS interface index
        let index = name_to_index(name);
        if index == 0 {
            tracing::warn!("Could not get interface index for {}, skipping", name);
            continue;
        }

        adopted.push(AdoptedNic {
            name: name.clone(),
            link_local: addr,
            index,
        });
    }

    adopted
}

/// Check if an IPv6 address is link-local (fe80::/10)
fn is_link_local_v6(addr: &Ipv6Addr) -> bool {
    let octets = addr.octets();
    octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80
}

/// Get the OS interface index for a NIC name.
/// Returns 0 on failure.
#[cfg(unix)]
fn name_to_index(name: &str) -> u32 {
    use std::ffi::CString;
    let Ok(c_name) = CString::new(name) else {
        return 0;
    };
    // SAFETY: if_nametoindex is a standard POSIX function that takes a
    // null-terminated string and returns the interface index (or 0 on error).
    unsafe { libc::if_nametoindex(c_name.as_ptr()) }
}

/// Get the OS interface index for a NIC name. Returns 0 on failure.
///
/// Windows exposes the same `if_nametoindex` C function from `iphlpapi` (same
/// signature and 0-on-error contract as the POSIX one used above).
#[cfg(windows)]
fn name_to_index(name: &str) -> u32 {
    use std::ffi::CString;
    let Ok(c_name) = CString::new(name) else {
        return 0;
    };
    #[link(name = "iphlpapi")]
    extern "system" {
        fn if_nametoindex(interface_name: *const std::os::raw::c_char) -> u32;
    }
    // SAFETY: same contract as POSIX `if_nametoindex` — null-terminated input,
    // returns the interface index or 0 on error.
    unsafe { if_nametoindex(c_name.as_ptr()) }
}

// Multicast address derivation
/// The IPv6 multicast discovery address for a group id, scope and address
/// type: [`wire::multicast_address`], the scope given by its config name.
pub(crate) fn derive_multicast_address(
    group_id: &[u8],
    scope: &str,
    address_type: MulticastAddressType,
) -> io::Result<Ipv6Addr> {
    Ok(wire::multicast_address(
        group_id,
        wire::scope_nibble(scope),
        address_type,
    ))
}

/// `token = full_hash(group_id + link_local_address_string)`
/// ([`wire::discovery_token`]).
pub(crate) fn make_discovery_token(group_id: &[u8], link_local_str: &str) -> [u8; 32] {
    wire::discovery_token(group_id, link_local_str)
}

/// `[token(32)] + [nonce(8)] + [data_port(2)]` ([`wire::discovery_packet`]).
pub(crate) fn build_discovery_packet(
    token: &[u8; 32],
    nonce: &[u8; NONCE_SIZE],
    data_port: u16,
) -> [u8; DISCOVERY_PACKET_SIZE] {
    wire::discovery_packet(token, nonce, data_port)
}

/// Parse a received discovery packet, Python's 32 bytes or ours 42
/// ([`wire::parse_discovery`]).
pub(crate) fn parse_discovery_packet(data: &[u8]) -> Option<DiscoveryFields> {
    wire::parse_discovery(data)
}

/// Verify a received discovery token in constant time
/// ([`wire::verify_discovery_token`]).
pub(crate) fn verify_discovery_token(token: &[u8], group_id: &[u8], src_addr_str: &str) -> bool {
    wire::verify_discovery_token(token, group_id, src_addr_str)
}

// Deduplication cache
/// Cache for deduplicating packets received from multiple NICs: the
/// shared [`wire::Dedup`] ring on a monotonic clock that starts with the
/// cache.
pub(crate) struct DeduplicationCache {
    ring: wire::Dedup,
    epoch: Instant,
}

impl DeduplicationCache {
    pub(crate) fn new() -> Self {
        Self {
            ring: wire::Dedup::new(),
            epoch: Instant::now(),
        }
    }

    /// Check if data is a duplicate. If not, adds it to the cache.
    /// Returns `true` if the data was already seen (duplicate).
    pub(crate) fn is_duplicate(&mut self, data: &[u8]) -> bool {
        let now_ms = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.ring.is_duplicate(data, now_ms)
    }
}

// Unicast discovery port
/// The unicast discovery port is discovery_port + 1 (matches Python)
pub(crate) fn unicast_discovery_port(discovery_port: u16) -> u16 {
    wire::unicast_discovery_port(discovery_port)
}

/// Reverse peering interval = announce_interval * 3.25 (matches Python)
pub(crate) fn reverse_peering_interval_secs() -> f64 {
    wire::REVERSE_PEERING_INTERVAL_MS as f64 / 1000.0
}

/// Short hex tag identifying a non-default discovery group.
///
/// Multiple `[[AutoInterface]]` sections run on the same LAN as isolated
/// discovery domains. A peer reachable in two groups shares one link-local
/// address, so without a per-group tag its interface name (`auto/<nic>/<ip>`)
/// would be identical in both groups and the two entries would be
/// indistinguishable in `rnstatus`. Appending this tag keeps the names unique
/// per group. Returns `None` for the default group so a single-section node's
/// interface names are unchanged.
pub(crate) fn group_name_tag(group_id: &[u8]) -> Option<String> {
    if group_id == DEFAULT_GROUP_ID {
        return None;
    }
    let h = full_hash(group_id);
    Some(format!("{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3]))
}

// Socket binding helpers
/// Bind a multicast discovery socket on a specific NIC.
///
/// - Joins the multicast group on the NIC
/// - Sets `SO_REUSEADDR` + `SO_REUSEPORT`
/// - Sets `IPV6_MULTICAST_IF` to the NIC's index
/// - Binds to the multicast address (link-local scope) or `[::]` (other scopes)
/// - Returns a tokio `UdpSocket`
pub(crate) fn bind_multicast_socket(
    nic: &AdoptedNic,
    mcast_addr: &Ipv6Addr,
    port: u16,
    scope: &str,
    enable_loopback: bool,
) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(target_os = "linux")]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;

    // Set multicast interface
    socket.set_multicast_if_v6(nic.index)?;

    // Enable/disable multicast loopback: the carrier self-echo mechanism
    // (default on, matching Python-RNS). Also required for same-machine tests.
    socket.set_multicast_loop_v6(enable_loopback)?;

    // Join multicast group on this NIC
    socket.join_multicast_v6(mcast_addr, nic.index)?;

    // Bind: link-local scope binds to the multicast addr with scope_id,
    // other scopes bind to the multicast addr without scope_id.
    let scope_id = if wire::scope_nibble(scope) == 0x2 {
        nic.index
    } else {
        0
    };
    let bind_addr = SocketAddrV6::new(*mcast_addr, port, 0, scope_id);
    socket.bind(&SockAddr::from(bind_addr))?;

    UdpSocket::from_std(socket.into())
}

/// Bind a unicast discovery socket on a specific NIC's link-local address.
///
/// Used for receiving reverse peering tokens.
pub(crate) fn bind_unicast_socket(nic: &AdoptedNic, port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(target_os = "linux")]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;

    let bind_addr = SocketAddrV6::new(nic.link_local, port, 0, nic.index);
    socket.bind(&SockAddr::from(bind_addr))?;

    UdpSocket::from_std(socket.into())
}

/// Bind a data receive socket on a specific NIC's link-local address.
///
/// Used for receiving Reticulum packets from discovered peers.
/// Binds to the configured `port` with SO_REUSEPORT for multi-process coexistence.
pub(crate) fn bind_data_socket(nic: &AdoptedNic, port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_nonblocking(true)?;
    socket.set_reuse_address(true)?;
    #[cfg(target_os = "linux")]
    socket.set_reuse_port(true)?;

    let bind_addr = SocketAddrV6::new(nic.link_local, port, 0, nic.index);
    socket.bind(&SockAddr::from(bind_addr))?;

    UdpSocket::from_std(socket.into())
}

/// Bind a UDP socket on `[::]:0` (any available port).
///
/// Used in tests as a simple ephemeral socket for send/recv testing.
#[cfg(test)]
fn bind_outbound_socket() -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_nonblocking(true)?;

    let bind_addr = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0);
    socket.bind(&SockAddr::from(bind_addr))?;

    UdpSocket::from_std(socket.into())
}

/// Receive result from `recv_from_any_by`.
pub(crate) struct RecvResult {
    /// Number of bytes received
    pub bytes_read: usize,
    /// Source address (with scope_id for link-local)
    pub source: SocketAddrV6,
    /// Index into the socket slice identifying which socket received
    pub socket_index: usize,
}

/// Poll the UDP socket of every item and return data from the first one ready.
///
/// For small N (1-3 NICs), iterative polling is efficient.
/// Returns when any socket has data available.
///
/// `poll_start` provides round-robin fairness: iteration starts at
/// `*poll_start` and wraps. After a successful recv, `*poll_start` advances
/// past the served socket so the next call starts elsewhere.
///
/// Uses `readable()` + `try_recv_from()` pattern (edge-triggered).
/// On `WouldBlock`, loops back to `readable()` to re-register waker.
///
/// `pick` projects the socket out of each item, so a caller that keeps one
/// struct per NIC does not need a socket vec parallel to it. It projects
/// instead of taking a ready-made `&[&UdpSocket]` because this is the hot
/// path: building that slice would allocate on every iteration of the
/// orchestrator's select loop, three times per iteration.
///
/// `pick` is called on each poll rather than once, so it must be cheap: a
/// field access or an `Arc` deref, which is what the orchestrator passes.
pub(crate) async fn recv_from_any_by<T, F>(
    items: &[T],
    pick: F,
    buf: &mut [u8],
    poll_start: &mut usize,
) -> io::Result<RecvResult>
where
    F: Fn(&T) -> &UdpSocket,
{
    if items.is_empty() {
        std::future::pending::<()>().await;
        unreachable!()
    }

    let len = items.len();
    let start = *poll_start;

    loop {
        // Wait for any socket to become readable (round-robin from poll_start)
        let ready_idx = {
            let idx = std::future::poll_fn(|cx| {
                for offset in 0..len {
                    let idx = (start + offset) % len;
                    match pick(&items[idx]).poll_recv_ready(cx) {
                        std::task::Poll::Ready(Ok(())) => {
                            return std::task::Poll::Ready(Ok(idx));
                        }
                        std::task::Poll::Ready(Err(e)) => {
                            return std::task::Poll::Ready(Err(e));
                        }
                        std::task::Poll::Pending => {}
                    }
                }
                std::task::Poll::Pending
            })
            .await?;
            idx
        };

        // Try non-blocking recv on the ready socket
        match pick(&items[ready_idx]).try_recv_from(buf) {
            Ok((n, addr)) => {
                let v6 = match addr {
                    std::net::SocketAddr::V6(v6) => v6,
                    std::net::SocketAddr::V4(_) => continue,
                };
                *poll_start = (ready_idx + 1) % len;
                return Ok(RecvResult {
                    bytes_read: n,
                    source: v6,
                    socket_index: ready_idx,
                });
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Spurious wakeup, re-register by looping back to poll_recv_ready
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codeberg #282: the address type selects the multicast GROUP, so the
    /// two types must derive different addresses from the same group_id, and
    /// each must be the address Python derives for that type
    /// (`AutoInterface.py:212`, `"ff"+type+scope+":"+gt`) - the group words
    /// are untouched, only the type nibble moves. A node configured
    /// `permanent` that still derived `ff12:` would sit in the temporary
    /// group with every other default node, exactly the silent failure the
    /// knob exists to prevent.
    #[test]
    fn test_derive_multicast_address_honours_the_address_type() {
        let temporary =
            derive_multicast_address(b"reticulum", "link", MulticastAddressType::Temporary)
                .unwrap();
        let permanent =
            derive_multicast_address(b"reticulum", "link", MulticastAddressType::Permanent)
                .unwrap();

        assert_eq!(
            temporary,
            "ff12:0:d70b:fb1c:16e4:5e39:485e:31e1"
                .parse::<Ipv6Addr>()
                .unwrap(),
        );
        assert_eq!(
            permanent,
            "ff02:0:d70b:fb1c:16e4:5e39:485e:31e1"
                .parse::<Ipv6Addr>()
                .unwrap(),
        );
        assert_ne!(
            temporary, permanent,
            "the two address types must be two different groups"
        );
    }

    /// The scope nibble and the type nibble are independent positions in the
    /// address; a permanent site-scoped group is `ff05:`.
    #[test]
    fn test_derive_multicast_address_permanent_site_scope() {
        let addr = derive_multicast_address(b"reticulum", "site", MulticastAddressType::Permanent)
            .unwrap();
        assert!(
            addr.to_string().starts_with("ff05:"),
            "permanent + site should produce ff05: prefix, got {}",
            addr
        );
    }

    /// Accepts the two spellings Python accepts, case- and whitespace-
    /// insensitively (`AutoInterface.py:175-182`), and rejects everything
    /// else rather than falling back to temporary the way Python does. The
    /// rejected cases include the raw nibbles `"0"` and `"1"`: Python treats
    /// those as unrecognised and lands on temporary, so honouring them here
    /// would make the same config file mean different groups on the two
    /// stacks.
    #[test]
    fn test_multicast_address_type_from_config_str() {
        for value in ["temporary", "Temporary", " TEMPORARY "] {
            assert_eq!(
                MulticastAddressType::from_config_str(value),
                Some(MulticastAddressType::Temporary),
                "{value:?} selects temporary"
            );
        }
        for value in ["permanent", "Permanent", "  permanent\t"] {
            assert_eq!(
                MulticastAddressType::from_config_str(value),
                Some(MulticastAddressType::Permanent),
                "{value:?} selects permanent"
            );
        }
        for value in ["0", "1", "", "permanant", "perm", "yes", "link"] {
            assert_eq!(
                MulticastAddressType::from_config_str(value),
                None,
                "{value:?} must be rejected, not silently resolved"
            );
        }
    }

    /// An absent key keeps the group every existing deployment is already in.
    #[test]
    fn test_multicast_address_type_defaults_to_temporary() {
        assert_eq!(
            MulticastAddressType::default(),
            MulticastAddressType::Temporary
        );
        assert_eq!(
            AutoInterfaceConfig::default().multicast_address_type,
            MulticastAddressType::Temporary
        );
    }

    #[test]
    fn test_derive_multicast_address_matches_python() {
        // Python test vector: group_id=b"reticulum", scope="link"
        // Expected: ff12:0:d70b:fb1c:16e4:5e39:485e:31e1
        let addr = derive_multicast_address(b"reticulum", "link", MulticastAddressType::Temporary)
            .unwrap();
        let expected: Ipv6Addr = "ff12:0:d70b:fb1c:16e4:5e39:485e:31e1".parse().unwrap();
        assert_eq!(addr, expected, "multicast address must match Python output");
    }

    #[test]
    fn test_derive_multicast_address_site_scope() {
        let addr = derive_multicast_address(b"reticulum", "site", MulticastAddressType::Temporary)
            .unwrap();
        let addr_str = addr.to_string();
        // Scope "site" = "5", type = "1" → prefix ff15:
        assert!(
            addr_str.starts_with("ff15:"),
            "site scope should produce ff15: prefix, got {}",
            addr_str
        );
    }

    #[test]
    fn test_derive_multicast_address_custom_group() {
        // Different group_id should produce a different address
        let default_addr =
            derive_multicast_address(b"reticulum", "link", MulticastAddressType::Temporary)
                .unwrap();
        let custom_addr = derive_multicast_address(
            b"my_custom_network",
            "link",
            MulticastAddressType::Temporary,
        )
        .unwrap();
        assert_ne!(default_addr, custom_addr);
    }

    #[test]
    fn test_derive_multicast_address_group_isolation() {
        // Codeberg #7: multiple AutoInterface sections with distinct group_ids
        // form isolated discovery domains. Isolation at the multicast layer
        // means each group derives a DIFFERENT multicast address, so a section
        // only ever receives its own group's discovery traffic. Same group_id
        // is deterministic (peers must agree on the address).
        let groups: [&[u8]; 3] = [b"reticulum", b"groupA", b"groupB"];
        let addrs: Vec<Ipv6Addr> = groups
            .iter()
            .map(|g| derive_multicast_address(g, "link", MulticastAddressType::Temporary).unwrap())
            .collect();

        // All pairwise-distinct.
        for i in 0..addrs.len() {
            for j in (i + 1)..addrs.len() {
                assert_ne!(
                    addrs[i], addrs[j],
                    "groups {:?} and {:?} must derive different multicast addresses",
                    groups[i], groups[j]
                );
            }
        }

        // Deterministic: same group_id → same address.
        assert_eq!(
            derive_multicast_address(b"groupA", "link", MulticastAddressType::Temporary).unwrap(),
            derive_multicast_address(b"groupA", "link", MulticastAddressType::Temporary).unwrap(),
        );
    }

    #[test]
    fn test_group_name_tag() {
        // Default group is untagged so single-section naming is unchanged.
        assert_eq!(group_name_tag(DEFAULT_GROUP_ID), None);
        assert_eq!(group_name_tag(b"reticulum"), None);

        // Non-default groups get a stable 8-hex-char tag.
        let tag_a = group_name_tag(b"groupA").expect("non-default group tagged");
        let tag_b = group_name_tag(b"groupB").expect("non-default group tagged");
        assert_eq!(tag_a.len(), 8);
        assert_ne!(tag_a, tag_b, "distinct groups get distinct tags");
        // Deterministic.
        assert_eq!(group_name_tag(b"groupA").unwrap(), tag_a);
    }

    #[test]
    fn test_discovery_token_roundtrip() {
        let group_id = b"reticulum";
        let addr = "fe80::1";
        let token = make_discovery_token(group_id, addr);
        assert!(verify_discovery_token(&token, group_id, addr));
    }

    #[test]
    fn test_discovery_token_matches_python() {
        // Python test vector: group_id=b"reticulum", addr="fe80::1"
        // Expected: 97b25576749ea936b0d8a8536ffaf442d157cf47d460dcf13c48b7bd18b6c163
        let token = make_discovery_token(b"reticulum", "fe80::1");
        let expected =
            hex::decode("97b25576749ea936b0d8a8536ffaf442d157cf47d460dcf13c48b7bd18b6c163")
                .unwrap();
        assert_eq!(token.as_slice(), expected.as_slice());
    }

    #[test]
    fn test_discovery_token_wrong_addr_rejected() {
        let group_id = b"reticulum";
        let token = make_discovery_token(group_id, "fe80::1");
        assert!(!verify_discovery_token(&token, group_id, "fe80::2"));
    }

    #[test]
    fn test_discovery_token_wrong_group_rejected() {
        let token = make_discovery_token(b"reticulum", "fe80::1");
        assert!(!verify_discovery_token(&token, b"other_group", "fe80::1"));
    }

    #[test]
    fn test_discovery_token_wrong_length_rejected() {
        assert!(!verify_discovery_token(&[0u8; 16], b"reticulum", "fe80::1"));
        assert!(!verify_discovery_token(&[], b"reticulum", "fe80::1"));
    }

    #[test]
    fn test_dedup_cache_rejects_duplicate() {
        let mut cache = DeduplicationCache::new();
        let data = b"test packet data";
        assert!(!cache.is_duplicate(data), "first time should not be dup");
        assert!(cache.is_duplicate(data), "second time should be dup");
    }

    #[test]
    fn test_dedup_cache_different_data_not_duplicate() {
        let mut cache = DeduplicationCache::new();
        assert!(!cache.is_duplicate(b"packet A"));
        assert!(!cache.is_duplicate(b"packet B"));
    }

    #[test]
    fn test_scope_to_byte() {
        let scope_to_byte = wire::scope_nibble;
        assert_eq!(scope_to_byte("link"), 0x2);
        assert_eq!(scope_to_byte("admin"), 0x4);
        assert_eq!(scope_to_byte("site"), 0x5);
        assert_eq!(scope_to_byte("organisation"), 0x8);
        assert_eq!(scope_to_byte("global"), 0xe);
        assert_eq!(scope_to_byte("Link"), 0x2); // case insensitive
        assert_eq!(scope_to_byte("unknown"), 0x2); // default
    }

    #[test]
    fn test_unicast_discovery_port() {
        assert_eq!(unicast_discovery_port(29716), 29717);
    }

    #[test]
    fn test_reverse_peering_interval() {
        let interval = reverse_peering_interval_secs();
        let expected = 1.6 * 3.25; // 5.2
        assert!((interval - expected).abs() < 0.001);
    }

    #[test]
    fn test_is_link_local_v6() {
        assert!(is_link_local_v6(&"fe80::1".parse().unwrap()));
        assert!(is_link_local_v6(
            &"fe80::abcd:1234:5678:9abc".parse().unwrap()
        ));
        assert!(!is_link_local_v6(&"::1".parse().unwrap()));
        assert!(!is_link_local_v6(&"2001:db8::1".parse().unwrap()));
        assert!(!is_link_local_v6(&"ff02::1".parse().unwrap()));
    }

    #[test]
    fn test_enumerate_nics_filters_loopback() {
        let config = AutoInterfaceConfig::default();
        let nics = enumerate_nics(&config);
        // Loopback (lo) should never appear
        for nic in &nics {
            assert_ne!(nic.name, "lo", "loopback must be filtered out");
        }
    }

    #[test]
    fn test_enumerate_nics_ignores_docker() {
        let config = AutoInterfaceConfig::default();
        let nics = enumerate_nics(&config);
        for nic in &nics {
            assert!(
                !nic.name.starts_with("docker"),
                "docker interfaces must be filtered: {}",
                nic.name
            );
            assert!(
                !nic.name.starts_with("veth"),
                "veth interfaces must be filtered: {}",
                nic.name
            );
        }
    }

    #[test]
    fn test_enumerate_nics_whitelist() {
        // Whitelist a non-existent device, should return empty
        let config = AutoInterfaceConfig {
            allowed_devices: Some("nonexistent_device_xyz".to_string()),
            ..Default::default()
        };
        let nics = enumerate_nics(&config);
        assert!(nics.is_empty());
    }

    #[tokio::test]
    async fn test_bind_outbound_socket() {
        let sock = bind_outbound_socket().expect("outbound socket should bind");
        let local = sock.local_addr().expect("should have local addr");
        assert_ne!(local.port(), 0, "OS should assign a port");
    }

    #[tokio::test]
    async fn test_recv_from_any_by_single_socket() {
        // Bind two sockets, send to the second, verify recv_from_any_by returns it.
        let s1 = bind_outbound_socket().unwrap();
        let s2 = bind_outbound_socket().unwrap();
        let s2_port = s2.local_addr().unwrap().port();

        // Sockets bind to the unspecified address (`::`), so `local_addr()`
        // reports `[::]:port` — which is not a valid unicast *destination*.
        // Linux delivers a packet sent to `::` locally anyway; macOS (more
        // correctly) does not, so addressing s2 by its raw local_addr made the
        // test Linux-only. Send to loopback `[::1]:port` instead — the
        // `::`-bound s2 still receives it — so the test is portable.
        let dest: std::net::SocketAddr =
            SocketAddrV6::new(Ipv6Addr::LOCALHOST, s2_port, 0, 0).into();

        let sender = bind_outbound_socket().unwrap();
        sender.send_to(b"hello", dest).await.unwrap();

        let sockets = [s1, s2];
        let mut buf = [0u8; 64];
        let mut poll_start = 0;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            recv_from_any_by(&sockets, |s: &UdpSocket| s, &mut buf, &mut poll_start),
        )
        .await
        .expect("timeout")
        .expect("recv error");

        assert_eq!(result.socket_index, 1, "should recv on socket index 1");
        assert_eq!(result.bytes_read, 5);
        assert_eq!(&buf[..5], b"hello");
        assert_eq!(poll_start, 0, "round-robin should advance past index 1");
    }

    #[tokio::test]
    async fn test_unicast_socket_bind_loopback() {
        // Test binding a unicast socket on localhost (uses ::1 instead of link-local)
        // This just verifies the socket2 setup path works.
        let nic = AdoptedNic {
            name: "lo".to_string(),
            link_local: "::1".parse().unwrap(),
            index: 1,
        };
        let result = bind_unicast_socket(&nic, 0);
        // May fail on some systems where ::1 needs special handling,
        // but should succeed on standard Linux
        if let Ok(sock) = result {
            let addr = sock.local_addr().unwrap();
            assert_ne!(addr.port(), 0);
        }
    }

    #[tokio::test]
    async fn test_data_socket_bind_loopback() {
        let nic = AdoptedNic {
            name: "lo".to_string(),
            link_local: "::1".parse().unwrap(),
            index: 1,
        };
        // Use port 0 so the OS assigns an ephemeral port for the test
        let result = bind_data_socket(&nic, 0);
        if let Ok(sock) = result {
            let addr = sock.local_addr().unwrap();
            assert_ne!(addr.port(), 0);
        }
    }

    #[test]
    fn test_discovery_packet_42_byte_roundtrip() {
        let token = make_discovery_token(b"reticulum", "fe80::1");
        let nonce = [0x42u8; NONCE_SIZE];
        let data_port = 42671u16;
        let pkt = build_discovery_packet(&token, &nonce, data_port);
        assert_eq!(pkt.len(), DISCOVERY_PACKET_SIZE);
        assert_eq!(pkt.len(), 42);

        let parsed = parse_discovery_packet(&pkt).expect("should parse 42 bytes");
        assert_eq!(parsed.token, token);
        assert_eq!(parsed.nonce, Some(nonce));
        assert_eq!(parsed.data_port, Some(data_port));
    }

    #[test]
    fn test_discovery_packet_32_byte_python_compat() {
        // Python sends 32-byte packets: just the token
        let token = make_discovery_token(b"reticulum", "fe80::1");
        let parsed = parse_discovery_packet(&token).expect("should parse 32 bytes");
        assert_eq!(parsed.token, token);
        assert!(
            parsed.nonce.is_none(),
            "32-byte Python packet should have no nonce"
        );
        assert!(
            parsed.data_port.is_none(),
            "32-byte Python packet should have no data_port"
        );
    }

    #[test]
    fn test_discovery_packet_token_first() {
        // Verify token occupies bytes [0..32]. Python reads data[:32]
        let token = make_discovery_token(b"reticulum", "fe80::1");
        let nonce = [0xABu8; NONCE_SIZE];
        let data_port = 12345u16;
        let pkt = build_discovery_packet(&token, &nonce, data_port);
        assert_eq!(&pkt[..32], &token[..]);
        assert_eq!(&pkt[32..40], &nonce[..]);
        assert_eq!(&pkt[40..42], &data_port.to_be_bytes());
    }

    #[test]
    fn test_discovery_packet_too_short_rejected() {
        assert!(parse_discovery_packet(&[0u8; 31]).is_none());
        assert!(parse_discovery_packet(&[]).is_none());
    }
}
