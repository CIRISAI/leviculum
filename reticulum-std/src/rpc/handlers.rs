//! RPC command dispatch: maps requests to node state queries

use std::sync::atomic::Ordering;

use reticulum_core::constants::TRUNCATED_HASHBYTES;
use serde_pickle::value::Value;

use super::error::RpcError;
use super::pickle::*;
use crate::driver::StdNodeCore;
use crate::interfaces::InterfaceStatsMap;

/// Dispatch an RPC request against node state and return the pickle-encoded response.
pub(super) fn handle_request(
    request: &RpcRequest,
    core: &mut StdNodeCore,
    start_time: std::time::Instant,
    iface_stats_map: &InterfaceStatsMap,
    auto_peer_count: usize,
    codec: Codec,
) -> Result<Vec<u8>, RpcError> {
    let response = match request {
        // Full implementations
        RpcRequest::GetInterfaceStats => {
            build_interface_stats(core, start_time, iface_stats_map, auto_peer_count)
        }
        RpcRequest::GetLinkCount => pickle_int(core.active_link_count() as i64),
        RpcRequest::GetPathTable { max_hops } => build_path_table(core, start_time, *max_hops),
        RpcRequest::GetRateTable => build_rate_table(core, start_time),
        RpcRequest::GetNextHop { destination_hash } => get_next_hop(core, destination_hash),
        RpcRequest::GetNextHopIfName { destination_hash } => {
            get_next_hop_if_name(core, destination_hash)
        }
        RpcRequest::GetFirstHopTimeout { .. } => {
            // Python DEFAULT_PER_HOP_TIMEOUT = 6 seconds
            pickle_float(6.0)
        }
        RpcRequest::DropPath { destination_hash } => drop_path(core, destination_hash),
        RpcRequest::DropAllVia { destination_hash } => drop_all_via(core, destination_hash),
        RpcRequest::DropAnnounceQueues => pickle_bool(true),

        // Radio-only (always None for TCP/UDP/Auto)
        RpcRequest::GetPacketRssi { .. } => pickle_none(),
        RpcRequest::GetPacketSnr { .. } => pickle_none(),
        RpcRequest::GetPacketQ { .. } => pickle_none(),

        // Blackhole stubs
        RpcRequest::GetBlackholedIdentities => pickle_dict(vec![]),
        RpcRequest::BlackholeIdentity { .. } => pickle_bool(true),
        RpcRequest::UnblackholeIdentity { .. } => pickle_bool(true),
    };

    serialize_response(&response, codec)
}

// Interface Stats (rnstatus)
/// Build the `interface_stats` response dict matching Python's format.
fn build_interface_stats(
    core: &StdNodeCore,
    start_time: std::time::Instant,
    iface_stats_map: &InterfaceStatsMap,
    auto_peer_count: usize,
) -> Value {
    let stats = core.interface_stats();
    let identity = core.identity();
    let transport_enabled = core.transport_config().enable_transport;
    let uptime = start_time.elapsed().as_secs_f64();
    let counters_map = iface_stats_map.lock().unwrap();
    let ifac_configs = core.clone_ifac_configs();

    // Count local clients for the "clients" field on LocalInterface
    let local_client_count = stats.iter().filter(|e| e.is_local_client).count();

    let mut total_rxb: u64 = 0;
    let mut total_txb: u64 = 0;
    let mut total_rxs: f64 = 0.0;
    let mut total_txs: f64 = 0.0;

    let mut iface_list = Vec::new();
    for entry in &stats {
        // Skip local client interfaces from the stats display
        // (Python also hides the LocalClientInterface from rnstatus)
        if entry.is_local_client {
            continue;
        }

        let iface_hash = compute_interface_hash(&entry.name);
        let itype = interface_type(&entry.name);

        // Read byte counters and compute speeds from the shared counters
        let (rxb, txb, rxs, txs) = counters_map
            .get(&entry.id)
            .map(|c| {
                let (rxs, txs) = c.speeds();
                (
                    c.rx_bytes.load(Ordering::Relaxed),
                    c.tx_bytes.load(Ordering::Relaxed),
                    rxs,
                    txs,
                )
            })
            .unwrap_or((0, 0, 0.0, 0.0));
        total_rxb += rxb;
        total_txb += txb;
        total_rxs += rxs;
        total_txs += txs;

        // Bitrate per interface type (matching Python's BITRATE_GUESS)
        let bitrate = match itype.as_str() {
            "LocalInterface" => pickle_int(1_000_000_000),
            _ => pickle_int(10_000_000),
        };

        // Clients field: only meaningful for LocalInterface server
        let clients = if itype == "LocalInterface" {
            pickle_int(local_client_count as i64)
        } else {
            pickle_none()
        };

        // Peers field: only meaningful for AutoInterface
        let peers = if itype == "AutoInterface" {
            pickle_int(auto_peer_count as i64)
        } else {
            pickle_none()
        };

        let iface_dict = pickle_dict(vec![
            (pickle_str_key("name"), pickle_str(&entry.name)),
            (
                pickle_str_key("short_name"),
                pickle_str(&short_name(&entry.name)),
            ),
            (pickle_str_key("hash"), pickle_bytes(&iface_hash)),
            (pickle_str_key("type"), pickle_str(&itype)),
            (pickle_str_key("rxb"), pickle_int(rxb as i64)),
            (pickle_str_key("txb"), pickle_int(txb as i64)),
            (pickle_str_key("rxs"), pickle_float(rxs)),
            (pickle_str_key("txs"), pickle_float(txs)),
            (pickle_str_key("status"), pickle_bool(true)),
            (pickle_str_key("mode"), pickle_int(0x01)), // MODE_FULL
            (pickle_str_key("bitrate"), bitrate),
            (pickle_str_key("clients"), clients),
            (pickle_str_key("peers"), peers),
            (
                pickle_str_key("incoming_announce_frequency"),
                pickle_float(entry.incoming_announce_frequency),
            ),
            (
                pickle_str_key("outgoing_announce_frequency"),
                pickle_float(entry.outgoing_announce_frequency),
            ),
            (pickle_str_key("held_announces"), pickle_int(0)),
            (pickle_str_key("announce_queue"), pickle_none()),
            (pickle_str_key("ifac_signature"), pickle_none()),
            (
                pickle_str_key("ifac_size"),
                match ifac_configs.get(&entry.id) {
                    Some(cfg) => pickle_int((cfg.ifac_size() * 8) as i64),
                    None => pickle_none(),
                },
            ),
            (pickle_str_key("ifac_netname"), pickle_none()),
        ]);

        iface_list.push(iface_dict);
    }

    let mut entries = vec![
        (pickle_str_key("interfaces"), pickle_list(iface_list)),
        (pickle_str_key("rxb"), pickle_int(total_rxb as i64)),
        (pickle_str_key("txb"), pickle_int(total_txb as i64)),
        (pickle_str_key("rxs"), pickle_float(total_rxs)),
        (pickle_str_key("txs"), pickle_float(total_txs)),
        (pickle_str_key("rss"), pickle_none()),
    ];

    if transport_enabled {
        entries.push((
            pickle_str_key("transport_id"),
            pickle_bytes(identity.hash()),
        ));
        entries.push((pickle_str_key("transport_uptime"), pickle_float(uptime)));
        let probe_value = match core.probe_dest_hash() {
            Some(hash) => pickle_bytes(hash.as_bytes()),
            None => pickle_none(),
        };
        entries.push((pickle_str_key("probe_responder"), probe_value));
        entries.push((pickle_str_key("network_id"), pickle_none()));
    }

    pickle_dict(entries)
}

// Path Table (rnpath -t)
/// Build the path table response. Timestamps are converted from monotonic core
/// milliseconds to approximate Unix epoch seconds using the start_time anchor.
fn build_path_table(
    core: &StdNodeCore,
    start_time: std::time::Instant,
    max_hops: Option<i64>,
) -> Value {
    let entries = core.path_table_entries();
    let iface_stats = core.interface_stats();
    let path_expiry_ms = core.transport_config().path_expiry_secs * 1000;

    // Anchor: wall clock at start_time
    let epoch_base = epoch_base_secs(start_time);
    let now_mono_ms = core.now_ms();

    let mut list = Vec::new();
    for entry in &entries {
        // Hops are now incremented on receipt (matching Python semantics)
        let python_hops = entry.hops as i64;
        if let Some(max) = max_hops {
            if python_hops > max {
                continue;
            }
        }

        let iface_name = iface_stats
            .iter()
            .find(|s| s.id == entry.interface_index)
            .map(|s| s.name.as_str())
            .unwrap_or("unknown");

        // Back-compute creation timestamp from expires - path_lifetime
        let timestamp_mono_ms = entry.expires_ms.saturating_sub(path_expiry_ms);
        let timestamp_secs = mono_ms_to_epoch(epoch_base, now_mono_ms, timestamp_mono_ms);
        let expires_secs = mono_ms_to_epoch(epoch_base, now_mono_ms, entry.expires_ms);

        let dict = pickle_dict(vec![
            (pickle_str_key("hash"), pickle_bytes(&entry.hash)),
            (pickle_str_key("timestamp"), pickle_float(timestamp_secs)),
            (
                pickle_str_key("via"),
                match &entry.next_hop {
                    // Relayed: next_hop is the relay's transport ID
                    Some(h) => pickle_bytes(h),
                    // Direct: Python uses the destination hash as received_from
                    // (Transport.py:1600), never None, rnpath crashes on None.
                    None => pickle_bytes(&entry.hash),
                },
            ),
            (pickle_str_key("hops"), pickle_int(python_hops)),
            (pickle_str_key("expires"), pickle_float(expires_secs)),
            (pickle_str_key("interface"), pickle_str(iface_name)),
        ]);
        list.push(dict);
    }
    pickle_list(list)
}

// Rate Table (rnpath -r)
fn build_rate_table(core: &StdNodeCore, start_time: std::time::Instant) -> Value {
    let entries = core.rate_table_entries();
    let epoch_base = epoch_base_secs(start_time);
    let now_mono_ms = core.now_ms();

    let mut list = Vec::new();
    for entry in &entries {
        let last_secs = mono_ms_to_epoch(epoch_base, now_mono_ms, entry.last_ms);
        let blocked_until_secs = if entry.blocked_until_ms > 0 {
            pickle_float(mono_ms_to_epoch(
                epoch_base,
                now_mono_ms,
                entry.blocked_until_ms,
            ))
        } else {
            pickle_float(0.0)
        };

        let dict = pickle_dict(vec![
            (pickle_str_key("hash"), pickle_bytes(&entry.hash)),
            (pickle_str_key("last"), pickle_float(last_secs)),
            (
                pickle_str_key("rate_violations"),
                pickle_int(entry.rate_violations as i64),
            ),
            (pickle_str_key("blocked_until"), blocked_until_secs),
            (pickle_str_key("timestamps"), pickle_list(vec![])),
        ]);
        list.push(dict);
    }
    pickle_list(list)
}

// Path Lookups (rnpath)
fn get_next_hop(core: &StdNodeCore, destination_hash: &[u8]) -> Value {
    let hash = match try_into_hash(destination_hash) {
        Some(h) => h,
        None => return pickle_none(),
    };
    match core.get_path_clone(&hash) {
        Some(entry) => match &entry.next_hop {
            Some(h) => pickle_bytes(h),
            // Direct path: Python returns destination_hash (Transport.py:1600)
            None => pickle_bytes(&hash),
        },
        None => pickle_none(),
    }
}

fn get_next_hop_if_name(core: &StdNodeCore, destination_hash: &[u8]) -> Value {
    let hash = match try_into_hash(destination_hash) {
        Some(h) => h,
        None => return pickle_str("unknown"),
    };
    match core.get_path_clone(&hash) {
        Some(entry) => {
            let iface_name = core
                .interface_stats()
                .iter()
                .find(|s| s.id == entry.interface_index)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "unknown".into());
            pickle_str(&iface_name)
        }
        None => pickle_str("unknown"),
    }
}

// Drop Operations
fn drop_path(core: &mut StdNodeCore, destination_hash: &[u8]) -> Value {
    let hash = match try_into_hash(destination_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    pickle_bool(core.remove_path(&hash))
}

fn drop_all_via(core: &mut StdNodeCore, via_hash: &[u8]) -> Value {
    let hash = match try_into_hash(via_hash) {
        Some(h) => h,
        None => return pickle_int(0),
    };
    pickle_int(core.drop_all_paths_via(&hash) as i64)
}

// Helpers
/// Compute a 16-byte interface hash from its name (matches Python Identity.full_hash).
fn compute_interface_hash(name: &str) -> [u8; 16] {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(name.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&hash[..16]);
    out
}

/// Extract short name from a full interface name.
/// E.g. "AutoInterface[Default Interface]" -> "Default Interface"
/// E.g. "tcp_client_0" -> "tcp_client_0"
fn short_name(name: &str) -> String {
    if let Some(start) = name.find('[') {
        if let Some(end) = name.find(']') {
            if start < end {
                return name[start + 1..end].to_string();
            }
        }
    }
    name.to_string()
}

/// Infer interface type from name.
fn interface_type(name: &str) -> String {
    if name.starts_with("AutoInterface") || name.starts_with("auto/") {
        "AutoInterface".to_string()
    } else if name.starts_with("tcp_client") || name.starts_with("TCPClient") {
        "TCPClientInterface".to_string()
    } else if name.starts_with("tcp_server") || name.starts_with("TCPServer") {
        "TCPServerInterface".to_string()
    } else if name.starts_with("udp") || name.starts_with("UDP") {
        "UDPInterface".to_string()
    } else if name.starts_with("local") || name.starts_with("Local") {
        "LocalInterface".to_string()
    } else {
        "Interface".to_string()
    }
}

/// Try to convert a byte slice to a 16-byte hash.
fn try_into_hash(bytes: &[u8]) -> Option<[u8; TRUNCATED_HASHBYTES]> {
    if bytes.len() >= TRUNCATED_HASHBYTES {
        let mut h = [0u8; TRUNCATED_HASHBYTES];
        h.copy_from_slice(&bytes[..TRUNCATED_HASHBYTES]);
        Some(h)
    } else {
        None
    }
}

/// Compute Unix epoch base from the monotonic start_time.
///
/// `start_time` is a `std::time::Instant` captured when the node was created.
/// `std::time::SystemTime::now() - start_time.elapsed()` gives the wall clock
/// at the moment `start_time` was created.
fn epoch_base_secs(start_time: std::time::Instant) -> f64 {
    let elapsed = start_time.elapsed();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now_epoch - elapsed).as_secs_f64()
}

/// Convert a core monotonic millisecond timestamp to Unix epoch seconds.
fn mono_ms_to_epoch(epoch_base: f64, _now_mono_ms: u64, mono_ms: u64) -> f64 {
    epoch_base + (mono_ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_name_with_brackets() {
        assert_eq!(
            short_name("AutoInterface[Default Interface]"),
            "Default Interface"
        );
    }

    #[test]
    fn test_short_name_without_brackets() {
        assert_eq!(short_name("tcp_client_0"), "tcp_client_0");
    }

    #[test]
    fn test_interface_type_auto() {
        assert_eq!(interface_type("AutoInterface[foo]"), "AutoInterface");
    }

    #[test]
    fn test_interface_type_auto_peer() {
        assert_eq!(interface_type("auto/eth0/abcd1234"), "AutoInterface");
    }

    #[test]
    fn test_interface_type_tcp_client() {
        assert_eq!(interface_type("tcp_client_0"), "TCPClientInterface");
    }

    #[test]
    fn test_interface_type_unknown() {
        assert_eq!(interface_type("custom_iface"), "Interface");
    }

    #[test]
    fn test_interface_hash_deterministic() {
        let h1 = compute_interface_hash("test");
        let h2 = compute_interface_hash("test");
        assert_eq!(h1, h2);
        assert_ne!(h1, [0u8; 16]);
    }

    #[test]
    fn test_try_into_hash() {
        assert!(try_into_hash(&[0xAB; 16]).is_some());
        assert!(try_into_hash(&[0xAB; 20]).is_some());
        assert!(try_into_hash(&[0xAB; 15]).is_none());
        assert!(try_into_hash(&[]).is_none());
    }
}
