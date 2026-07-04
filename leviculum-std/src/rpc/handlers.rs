//! RPC command dispatch: maps requests to node state queries

use std::sync::atomic::Ordering;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use serde_pickle::value::{HashableValue, Value};

use super::error::RpcError;
use super::pickle::*;
use crate::driver::StdNodeCore;
use crate::interfaces::{InterfaceOnlineMap, InterfaceStatsMap};

/// Dispatch an RPC request against node state and return the pickle-encoded response.
pub(super) fn handle_request(
    request: &RpcRequest,
    core: &mut StdNodeCore,
    start_time: std::time::Instant,
    iface_stats_map: &InterfaceStatsMap,
    iface_online_map: &InterfaceOnlineMap,
    auto_peer_count: usize,
    codec: Codec,
) -> Result<Vec<u8>, RpcError> {
    let response = match request {
        // Full implementations
        RpcRequest::GetInterfaceStats => build_interface_stats(
            core,
            start_time,
            iface_stats_map,
            iface_online_map,
            auto_peer_count,
        ),
        RpcRequest::GetLinkCount => pickle_int(core.active_link_count() as i64),
        RpcRequest::GetLinkTable => build_link_table(core),
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

        // Blackhole set (Codeberg #67 + #88). Real membership-backed set on
        // Transport; matches the Python wire contract (Reticulum.py:1699-1742,
        // Transport.py:3409-3448). Enforcement mirrors Python's three read
        // sites: inbound announces from blackholed identities are dropped
        // (Identity.py:574-577), matching path-table rows are removed on
        // blackhole (Transport.py:3423, 3494-3513), and incoming links that
        // identify as a blackholed identity are torn down (Link.py:1021-1023).
        // Expired `until` entries are swept from the driver's timer branch
        // (Transport.py:973-994).
        RpcRequest::GetBlackholedIdentities => build_blackholed_identities(core),
        RpcRequest::BlackholeIdentity {
            identity_hash,
            until,
            reason,
        } => blackhole_identity(core, identity_hash, *until, reason.clone()),
        RpcRequest::UnblackholeIdentity { identity_hash } => {
            unblackhole_identity(core, identity_hash)
        }
        RpcRequest::IsBlackholed { identity_hash } => is_blackholed(core, identity_hash),

        // destination_data cache lifecycle (Codeberg #84).
        //
        // Upstream Reticulum b5658c4 (2026-04-20, "Keep track of which
        // known destinations are actually in use, so irrelevant
        // destination data can be cleaned") added a known_destinations
        // GC scheme exposed via three RPC ops on the destination_data
        // dict key: "used", "retain", "unretain".  These map onto our
        // announce_cache: "retain" pins an entry so clean_announce_cache
        // never evicts it, "unretain" lifts the pin, "used" is a recency
        // touch that skips pinned entries — the same use-state semantics
        // as Python's known_destinations[dest][4].  A Python tool driving
        // lnsd sees the same booleans and the same survival-under-pressure
        // behaviour as against a Python rnsd.
        RpcRequest::DestinationDataUsed { destination_hash } => {
            destination_data_used(core, destination_hash)
        }
        RpcRequest::DestinationDataRetain { destination_hash } => {
            destination_data_retain(core, destination_hash)
        }
        RpcRequest::DestinationDataUnretain { destination_hash } => {
            destination_data_unretain(core, destination_hash)
        }

        // identity_data cache lifecycle (Codeberg #84).
        //
        // rnid retains an identity after a successful recall via
        // Reticulum._retain_identity, which issues
        // `{"identity_data": "retain", "identity_hash": <bytes>}` to the
        // shared instance (Reticulum.py:1316). Upstream rnsd dispatches it
        // to Identity._retain_identity, which retains every known
        // destination whose public key hashes to that identity. We mirror
        // that: retain/unretain pin/unpin all announce_cache destinations
        // matching the identity. The unretain arm is symmetric though
        // current upstream only emits retain.
        RpcRequest::IdentityDataRetain { identity_hash } => {
            identity_data_retain(core, identity_hash)
        }
        RpcRequest::IdentityDataUnretain { identity_hash } => {
            identity_data_unretain(core, identity_hash)
        }
    };

    serialize_response(&response, codec)
}

// Interface Stats (rnstatus)
/// Map a resolved announce-rate value to a pickle scalar: `Some(v)` -> int,
/// `None` -> Python `None` (Codeberg #67 Stage 2a).
fn ar_value(v: Option<u32>) -> Value {
    match v {
        Some(v) => pickle_int(v as i64),
        None => pickle_none(),
    }
}

/// Build the RNode radio-stats key/value entries for one interface's
/// `interface_stats` dict (Codeberg #25).
///
/// Field names and units match Python `Reticulum.get_interface_stats`
/// (Reticulum.py:1371-1420) so rnstatus/lnstatus render the radio rows without
/// special-casing:
///   - `airtime_short`/`airtime_long`, `channel_load_short`/`channel_load_long`
///     -> float percent
///   - `noise_floor` -> int dBm, or `None`
///   - `cpu_temp` -> int Celsius (from `CMD_STAT_TEMP`), or `None`
///   - `battery_state` (string) / `battery_percent` (int) -> only once the
///     reported state leaves `Unknown` (Python emits these keys only when
///     `r_battery_state != 0x00`).
fn radio_stat_fields(r: &crate::interfaces::RadioStats) -> Vec<(HashableValue, Value)> {
    let opt_int = |v: Option<i16>| match v {
        Some(x) => pickle_int(x as i64),
        None => pickle_none(),
    };
    let mut fields = vec![
        (
            pickle_str_key("airtime_short"),
            pickle_float(r.airtime_short),
        ),
        (pickle_str_key("airtime_long"), pickle_float(r.airtime_long)),
        (
            pickle_str_key("channel_load_short"),
            pickle_float(r.channel_load_short),
        ),
        (
            pickle_str_key("channel_load_long"),
            pickle_float(r.channel_load_long),
        ),
        (pickle_str_key("noise_floor"), opt_int(r.noise_floor)),
        (pickle_str_key("cpu_temp"), opt_int(r.cpu_temp)),
    ];
    if r.battery_state != leviculum_core::rnode::BatteryState::Unknown {
        fields.push((
            pickle_str_key("battery_state"),
            pickle_str(r.battery_state.as_str()),
        ));
        fields.push((
            pickle_str_key("battery_percent"),
            pickle_int(r.battery_percent as i64),
        ));
    }
    fields
}

/// Build the `interface_stats` response dict matching Python's format.
/// `core` is mutable because frequency reads pop decayed samples, exactly
/// like Python's get_interface_stats (Python parity, Codeberg #67/#87).
pub(crate) fn build_interface_stats(
    core: &mut StdNodeCore,
    start_time: std::time::Instant,
    iface_stats_map: &InterfaceStatsMap,
    iface_online_map: &InterfaceOnlineMap,
    auto_peer_count: usize,
) -> Value {
    let stats = core.interface_stats();
    let identity = core.identity();
    let transport_enabled = core.transport_config().enable_transport;
    let uptime = start_time.elapsed().as_secs_f64();
    let epoch_base = epoch_base_secs(start_time);
    let counters_map = iface_stats_map.lock().unwrap();
    let online_map = iface_online_map.lock().unwrap();
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

        // Codeberg #25: latest RNode radio stats (None for non-radio interfaces).
        let radio = counters_map.get(&entry.id).and_then(|c| c.radio_stats());

        // Bitrate reporting (Codeberg #93). A configured `bitrate` (fed into the
        // announce cap) overrides the per-type BITRATE_GUESS, matching Python's
        // `configured_bitrate` override (Reticulum.py:887/1421-1423). Unset falls
        // back to the medium default guess.
        let bitrate = match entry.configured_bitrate {
            Some(bps) => pickle_int(bps as i64),
            None => match itype.as_str() {
                "LocalInterface" => pickle_int(1_000_000_000),
                _ => pickle_int(10_000_000),
            },
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

        let mut iface_fields = vec![
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
            // status: real `Interface::is_online()` (Codeberg #56). Source of
            // truth is `iface_online_map`, populated by the driver on register
            // and cleared on disconnect. Missing entry → fall back to `true`
            // (preserves the pre-fix behavior for any caller-side mismatch).
            (
                pickle_str_key("status"),
                pickle_bool(online_map.get(&entry.id).copied().unwrap_or(true)),
            ),
            // mode: real Reticulum propagation mode (Codeberg #91), carried
            // per-interface by transport from the parsed config and reported
            // as the Python `Interface.MODE_*` value so rnstatus/lnstatus print
            // the right label (Utilities/rnstatus.py:421-427).
            (
                pickle_str_key("mode"),
                pickle_int(entry.mode.as_u8() as i64),
            ),
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
            // Codeberg #67 Stage 2a: incoming/outgoing_pr_frequency are now real,
            // measured from per-interface path-request deques (Python
            // ip_freq_deque / op_freq_deque). They read 0.0 on an under-filled
            // deque, matching Interface.incoming_pr_frequency()/
            // outgoing_pr_frequency() (Interface.py:301-321).
            (
                pickle_str_key("incoming_pr_frequency"),
                pickle_float(entry.incoming_pr_frequency),
            ),
            (
                pickle_str_key("outgoing_pr_frequency"),
                pickle_float(entry.outgoing_pr_frequency),
            ),
            // Codeberg #67 Stage 2a: announce_rate_target/penalty/grace now carry
            // the real per-interface config (Reticulum.py:798-833). Unset keys
            // fall back to the Python interface defaults (target=3600 s,
            // penalty=0 s, grace=5) when transport is enabled, and stay None when
            // transport is disabled. rnstatus renders the `(t:.../p:.../g:...)`
            // suffix only when target is truthy (rnstatus.py:556-563).
            (
                pickle_str_key("announce_rate_target"),
                ar_value(entry.announce_rate_target),
            ),
            (
                pickle_str_key("announce_rate_penalty"),
                ar_value(entry.announce_rate_penalty),
            ),
            (
                pickle_str_key("announce_rate_grace"),
                ar_value(entry.announce_rate_grace),
            ),
            // burst_active/activated + pr_burst_active/activated: real ingress
            //   limiter burst state (Codeberg #87), read from the per-interface
            //   IngressBurstState (Python ic_burst_active/ic_burst_activated and
            //   ic_pr_burst_active/ic_pr_burst_activated, Interface.py:115-118).
            //   Idle interfaces read False / 0. rnstatus only reads *_activated
            //   when the matching *_active is truthy (rnstatus.py:565-573), so an
            //   idle interface renders no burst suffix.
            //   The core records activation on its monotonic clock; Python
            //   reports time.time() and rnstatus renders `now - activated` as
            //   the burst duration (rnstatus.py:566), so convert to epoch
            //   seconds here. Idle stays the int 0 (Python's initial value).
            (
                pickle_str_key("burst_active"),
                pickle_bool(entry.burst_active),
            ),
            (
                pickle_str_key("burst_activated"),
                activation_to_epoch(epoch_base, entry.burst_activated),
            ),
            (
                pickle_str_key("pr_burst_active"),
                pickle_bool(entry.pr_burst_active),
            ),
            (
                pickle_str_key("pr_burst_activated"),
                activation_to_epoch(epoch_base, entry.pr_burst_activated),
            ),
            (
                pickle_str_key("held_announces"),
                pickle_int(entry.held_announces as i64),
            ),
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
        ];

        // Codeberg #25: RNode radio stats. Emitted only for radio interfaces
        // (Python gates each key on hasattr(interface, "r_*"), which is true
        // only for RNodeInterface).
        if let Some(r) = radio {
            iface_fields.extend(radio_stat_fields(&r));
        }

        let iface_dict = pickle_dict(iface_fields);

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

        // Name lookup only; interface_stats() would pop frequency samples.
        let iface_name = core
            .interface_name(entry.interface_index)
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

// Link Table (Leviculum-only `link_table` RPC — `lnstest diag` v2)
/// Build the `link_table` response — a list of per-link dicts.
///
/// One entry per local [`crate::link::Link`] regardless of state. Python
/// `rnsd` has no `link_table` precedent (it exposes `link_count` only); the
/// response shape is therefore a Leviculum extension, kept to
/// pickle-friendly scalars so Python clients can still deserialise it into
/// a list of dicts if they ever consume it.
fn build_link_table(core: &StdNodeCore) -> Value {
    let entries = core.link_table_entries();

    let mut list = Vec::new();
    for entry in &entries {
        // Name lookup only; interface_stats() would pop frequency samples.
        let iface_name = entry
            .interface_index
            .and_then(|idx| core.interface_name(idx))
            .unwrap_or("");
        let dict = pickle_dict(vec![
            (pickle_str_key("link_id"), pickle_bytes(&entry.link_id)),
            (pickle_str_key("state"), pickle_str(entry.state)),
            (
                pickle_str_key("destination_hash"),
                pickle_bytes(&entry.destination_hash),
            ),
            (
                pickle_str_key("age"),
                match entry.age_secs {
                    Some(s) => pickle_float(s as f64),
                    None => pickle_none(),
                },
            ),
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
            // Name lookup only; interface_stats() would pop frequency samples.
            let iface_name = core
                .interface_name(entry.interface_index)
                .unwrap_or("unknown");
            pickle_str(iface_name)
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

// Blackhole set (Codeberg #67)
/// Build the `blackholed_identities` response: a dict keyed by identity hash
/// (bytes) mapping to an entry dict `{"source", "until", "reason"}`, matching
/// Python's `RNS.Transport.blackholed_identities` (Transport.py:3420). The empty
/// case is an empty dict, exactly as the prior stub returned.
fn build_blackholed_identities(core: &StdNodeCore) -> Value {
    use serde_pickle::value::HashableValue;
    let entries = core
        .blackholed_identities()
        .iter()
        .map(|(hash, entry)| {
            let value = pickle_dict(vec![
                (pickle_str_key("source"), pickle_bytes(&entry.source)),
                (
                    pickle_str_key("until"),
                    entry.until.map(pickle_float).unwrap_or_else(pickle_none),
                ),
                (
                    pickle_str_key("reason"),
                    entry
                        .reason
                        .as_deref()
                        .map(pickle_str)
                        .unwrap_or_else(pickle_none),
                ),
            ]);
            (HashableValue::Bytes(hash.to_vec()), value)
        })
        .collect();
    Value::Dict(entries)
}

/// Insert into the blackhole set. Returns bool `true` on a fresh blackhole and
/// `None` when the identity was already present, mirroring Python's
/// `Transport.blackhole_identity` (Transport.py:3425/3427). An invalid hash
/// length yields `false`, matching the client-side length guard
/// (Reticulum.py:1723).
fn blackhole_identity(
    core: &mut StdNodeCore,
    identity_hash: &[u8],
    until: Option<f64>,
    reason: Option<String>,
) -> Value {
    let hash = match try_into_hash(identity_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    if core.blackhole_identity(hash, until, reason) {
        pickle_bool(true)
    } else {
        pickle_none()
    }
}

/// Remove from the blackhole set. Returns bool `true` when an entry was lifted
/// and `None` when the identity was not blackholed, mirroring Python's
/// `Transport.unblackhole_identity` (Transport.py:3446/3448).
fn unblackhole_identity(core: &mut StdNodeCore, identity_hash: &[u8]) -> Value {
    let hash = match try_into_hash(identity_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    if core.unblackhole_identity(&hash) {
        pickle_bool(true)
    } else {
        pickle_none()
    }
}

/// Membership check. Returns a bool, matching `identity_hash in
/// RNS.Transport.blackholed_identities` (Reticulum.py:1720).
fn is_blackholed(core: &StdNodeCore, identity_hash: &[u8]) -> Value {
    let hash = match try_into_hash(identity_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    pickle_bool(core.is_blackholed(&hash))
}

// Known-destination cache lifecycle (Codeberg #84)
/// Recency touch. Returns bool, mirroring Python
/// `Identity._used_destination_data`: true only when the destination is known
/// and not retained, false otherwise (including an invalid-length hash).
fn destination_data_used(core: &mut StdNodeCore, destination_hash: &[u8]) -> Value {
    let hash = match try_into_hash(destination_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    pickle_bool(core.used_destination_data(&hash))
}

/// Pin a known destination against cache eviction. Returns bool, mirroring
/// Python `Identity._retain_destination_data`.
fn destination_data_retain(core: &mut StdNodeCore, destination_hash: &[u8]) -> Value {
    let hash = match try_into_hash(destination_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    pickle_bool(core.retain_destination_data(&hash))
}

/// Lift a destination's retain pin. Returns bool, mirroring Python
/// `Identity._unretain_destination_data`.
fn destination_data_unretain(core: &mut StdNodeCore, destination_hash: &[u8]) -> Value {
    let hash = match try_into_hash(destination_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    pickle_bool(core.unretain_destination_data(&hash))
}

/// Retain every known destination for an identity. Returns bool, mirroring
/// Python `Identity._retain_identity` (true iff at least one was retained).
fn identity_data_retain(core: &mut StdNodeCore, identity_hash: &[u8]) -> Value {
    let hash = match try_into_hash(identity_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    pickle_bool(core.retain_identity_data(&hash))
}

/// Lift the retain pin on every known destination for an identity. Returns
/// bool (symmetric counterpart to [`identity_data_retain`]).
fn identity_data_unretain(core: &mut StdNodeCore, identity_hash: &[u8]) -> Value {
    let hash = match try_into_hash(identity_hash) {
        Some(h) => h,
        None => return pickle_bool(false),
    };
    pickle_bool(core.unretain_identity_data(&hash))
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

/// Burst activation timestamp for interface_stats: 0 (never activated) stays
/// the int 0 like Python's initial `ic_burst_activated`; a real activation is
/// converted from the core's monotonic seconds to epoch seconds, because
/// rnstatus renders `time.time() - burst_activated` as the burst duration
/// (rnstatus.py:566).
fn activation_to_epoch(epoch_base: f64, activated_mono_secs: u64) -> Value {
    if activated_mono_secs == 0 {
        pickle_int(0)
    } else {
        pickle_float(epoch_base + activated_mono_secs as f64)
    }
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

    // identity_data lifecycle dispatch end-to-end: handle_request must produce a
    // bool response (never drop the connection) for both the pickle and msgpack
    // codecs. For an identity with no known destinations (nothing cached), the
    // real handler reports Bool(false), mirroring Python `_retain_identity`
    // returning retained=False. This is the wire contract rnid relies on when it
    // issues identity_data:retain after a recall (it swallows the boolean).
    #[test]
    fn identity_data_handlers_return_bool_false_when_unknown() {
        use crate::clock::SystemClock;
        use crate::interfaces::{InterfaceOnlineMap, InterfaceStatsMap};
        use crate::rpc::pickle::{decode_response_msgpack, Codec, RpcRequest};
        use leviculum_core::node::NodeCoreBuilder;
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};

        let tmp = std::env::temp_dir().join(format!("rpc-identity-data-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut core: StdNodeCore = NodeCoreBuilder::new().enable_transport(true).build(
            rand_core::OsRng,
            SystemClock::new(),
            crate::storage::Storage::new(&tmp).unwrap(),
        );
        let stats: InterfaceStatsMap = Arc::new(Mutex::new(BTreeMap::new()));
        let online: InterfaceOnlineMap = Arc::new(Mutex::new(BTreeMap::new()));
        let start = std::time::Instant::now();

        let decode = |bytes: &[u8], codec: Codec| -> Value {
            match codec {
                Codec::Pickle => serde_pickle::value_from_slice(bytes, Default::default()).unwrap(),
                Codec::Msgpack => decode_response_msgpack(bytes).unwrap(),
            }
        };

        for codec in [Codec::Pickle, Codec::Msgpack] {
            for req in [
                RpcRequest::IdentityDataRetain {
                    identity_hash: vec![0x11u8; 16],
                },
                RpcRequest::IdentityDataUnretain {
                    identity_hash: vec![0x22u8; 16],
                },
            ] {
                let bytes =
                    handle_request(&req, &mut core, start, &stats, &online, 0, codec).unwrap();
                let value = decode(&bytes, codec);
                assert!(
                    matches!(value, Value::Bool(false)),
                    "{:?} via {:?} must return bool false for an unknown identity, got {:?}",
                    req,
                    codec,
                    value
                );
            }
        }
    }

    // destination_data cache-lifecycle RPC round-trip (Codeberg #84): each op
    // dispatches end-to-end and returns the real bool reflecting the known
    // destination's use-state, over both codecs.
    #[test]
    fn destination_data_rpc_round_trip() {
        use crate::clock::SystemClock;
        use crate::interfaces::{InterfaceOnlineMap, InterfaceStatsMap};
        use crate::rpc::pickle::{decode_response_msgpack, Codec, RpcRequest};
        use leviculum_core::node::NodeCoreBuilder;
        use leviculum_core::traits::Storage;
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};

        let tmp = std::env::temp_dir().join(format!("rpc-dest-data-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let stats: InterfaceStatsMap = Arc::new(Mutex::new(BTreeMap::new()));
        let online: InterfaceOnlineMap = Arc::new(Mutex::new(BTreeMap::new()));
        let start = std::time::Instant::now();

        let decode = |bytes: &[u8], codec: Codec| -> Value {
            match codec {
                Codec::Pickle => serde_pickle::value_from_slice(bytes, Default::default()).unwrap(),
                Codec::Msgpack => decode_response_msgpack(bytes).unwrap(),
            }
        };
        let dispatch = |core: &mut StdNodeCore, req: &RpcRequest, codec: Codec| -> Value {
            let bytes = handle_request(req, core, start, &stats, &online, 0, codec).unwrap();
            decode(&bytes, codec)
        };

        let dest = vec![0x42u8; 16];
        let dest_arr: [u8; 16] = dest.clone().try_into().unwrap();

        for codec in [Codec::Pickle, Codec::Msgpack] {
            let mut core: StdNodeCore = NodeCoreBuilder::new().enable_transport(true).build(
                rand_core::OsRng,
                SystemClock::new(),
                crate::storage::Storage::new(&tmp).unwrap(),
            );

            // Unknown destination: every op reports false.
            for op in [
                RpcRequest::DestinationDataUsed {
                    destination_hash: dest.clone(),
                },
                RpcRequest::DestinationDataRetain {
                    destination_hash: dest.clone(),
                },
                RpcRequest::DestinationDataUnretain {
                    destination_hash: dest.clone(),
                },
            ] {
                assert!(
                    matches!(dispatch(&mut core, &op, codec), Value::Bool(false)),
                    "{:?} on an unknown destination must be false ({:?})",
                    op,
                    codec
                );
            }

            // Make it known (cache a placeholder announce blob).
            core.storage_mut()
                .set_announce_cache(dest_arr, vec![0xAB; 32]);

            // used → true (recency touch), retain → true (pin), then used → false
            // because a retained entry is skipped, unretain → true (lifts pin),
            // used → true again.
            assert!(matches!(
                dispatch(
                    &mut core,
                    &RpcRequest::DestinationDataUsed {
                        destination_hash: dest.clone()
                    },
                    codec
                ),
                Value::Bool(true)
            ));
            assert!(matches!(
                dispatch(
                    &mut core,
                    &RpcRequest::DestinationDataRetain {
                        destination_hash: dest.clone()
                    },
                    codec
                ),
                Value::Bool(true)
            ));
            assert!(
                matches!(
                    dispatch(
                        &mut core,
                        &RpcRequest::DestinationDataUsed {
                            destination_hash: dest.clone()
                        },
                        codec
                    ),
                    Value::Bool(false)
                ),
                "used on a retained destination is false ({:?})",
                codec
            );
            assert!(matches!(
                dispatch(
                    &mut core,
                    &RpcRequest::DestinationDataUnretain {
                        destination_hash: dest.clone()
                    },
                    codec
                ),
                Value::Bool(true)
            ));
            assert!(matches!(
                dispatch(
                    &mut core,
                    &RpcRequest::DestinationDataUsed {
                        destination_hash: dest.clone()
                    },
                    codec
                ),
                Value::Bool(true)
            ));
        }
    }

    // Codeberg #25: the radio-stats field builder emits the Python field
    // names/units and gates battery_state/battery_percent on a known state.
    #[test]
    fn radio_stat_fields_names_units_and_gating() {
        use crate::interfaces::RadioStats;
        use leviculum_core::rnode::BatteryState;

        let get = |fields: &[(HashableValue, Value)], key: &str| -> Option<Value> {
            fields
                .iter()
                .find(|(k, _)| *k == HashableValue::String(key.into()))
                .map(|(_, v)| v.clone())
        };

        // Unknown battery + no reports: airtime/channel-load default to 0.0,
        // noise_floor/cpu_temp are None, battery_* keys are omitted.
        let f = radio_stat_fields(&RadioStats::default());
        assert_eq!(get(&f, "airtime_short"), Some(Value::F64(0.0)));
        assert_eq!(get(&f, "airtime_long"), Some(Value::F64(0.0)));
        assert_eq!(get(&f, "channel_load_short"), Some(Value::F64(0.0)));
        assert_eq!(get(&f, "channel_load_long"), Some(Value::F64(0.0)));
        assert_eq!(get(&f, "noise_floor"), Some(Value::None));
        assert_eq!(get(&f, "cpu_temp"), Some(Value::None));
        assert!(get(&f, "battery_state").is_none());
        assert!(get(&f, "battery_percent").is_none());

        // Populated values, charging battery.
        let r = RadioStats {
            airtime_short: 3.0,
            airtime_long: 10.0,
            channel_load_short: 2.0,
            channel_load_long: 6.0,
            noise_floor: Some(-57),
            cpu_temp: Some(25),
            battery_state: BatteryState::Charging,
            battery_percent: 85,
            last_rssi: Some(-57),
            last_snr: Some(10.0),
        };
        let f = radio_stat_fields(&r);
        assert_eq!(get(&f, "airtime_short"), Some(Value::F64(3.0)));
        assert_eq!(get(&f, "airtime_long"), Some(Value::F64(10.0)));
        assert_eq!(get(&f, "channel_load_short"), Some(Value::F64(2.0)));
        assert_eq!(get(&f, "channel_load_long"), Some(Value::F64(6.0)));
        assert_eq!(get(&f, "noise_floor"), Some(Value::I64(-57)));
        assert_eq!(get(&f, "cpu_temp"), Some(Value::I64(25)));
        assert_eq!(
            get(&f, "battery_state"),
            Some(Value::String("charging".into()))
        );
        assert_eq!(get(&f, "battery_percent"), Some(Value::I64(85)));
        // RSSI/SNR are stored on state but not surfaced in interface_stats
        // (Python does not place them in the dict either).
        assert!(get(&f, "last_rssi").is_none());
        assert!(get(&f, "r_stat_rssi").is_none());
    }

    // Codeberg #25: end-to-end through build_interface_stats — a radio
    // interface's response dict carries the radio rows with the right
    // fields/units.
    #[test]
    fn build_interface_stats_emits_radio_rows_for_rnode() {
        use crate::clock::SystemClock;
        use crate::interfaces::{InterfaceCounters, InterfaceOnlineMap, InterfaceStatsMap};
        use leviculum_core::node::NodeCoreBuilder;
        use leviculum_core::rnode::BatteryState;
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};

        let tmp = std::env::temp_dir().join(format!("rpc-radio-stats-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut core: StdNodeCore = NodeCoreBuilder::new().enable_transport(true).build(
            rand_core::OsRng,
            SystemClock::new(),
            crate::storage::Storage::new(&tmp).unwrap(),
        );
        core.set_interface_name(0, "RNodeInterface[/dev/ttyUSB0]".into());

        let counters = Arc::new(InterfaceCounters::new());
        counters.enable_radio_stats();
        counters.update_radio(|r| {
            r.airtime_short = 3.0;
            r.noise_floor = Some(-57);
            r.cpu_temp = Some(25);
            r.battery_state = BatteryState::Charging;
            r.battery_percent = 85;
        });
        let stats: InterfaceStatsMap = Arc::new(Mutex::new(BTreeMap::from([(0usize, counters)])));
        let online: InterfaceOnlineMap = Arc::new(Mutex::new(BTreeMap::new()));

        let value = build_interface_stats(&mut core, std::time::Instant::now(), &stats, &online, 0);

        let Value::Dict(top) = value else {
            panic!("interface_stats must be a dict")
        };
        let ifaces = top
            .get(&HashableValue::String("interfaces".into()))
            .expect("interfaces key");
        let Value::List(list) = ifaces else {
            panic!("interfaces must be a list")
        };
        assert_eq!(list.len(), 1, "the one registered interface must appear");
        let Value::Dict(iface) = &list[0] else {
            panic!("interface entry must be a dict")
        };
        let get = |k: &str| iface.get(&HashableValue::String(k.into())).cloned();
        assert_eq!(get("airtime_short"), Some(Value::F64(3.0)));
        assert_eq!(get("noise_floor"), Some(Value::I64(-57)));
        assert_eq!(get("cpu_temp"), Some(Value::I64(25)));
        assert_eq!(get("battery_state"), Some(Value::String("charging".into())));
        assert_eq!(get("battery_percent"), Some(Value::I64(85)));
    }
}
