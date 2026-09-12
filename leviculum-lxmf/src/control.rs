//! Propagation-node remote management: the control destination's request
//! paths and the wire codec for their payloads (Codeberg #384 part 4).
//!
//! The reference registers three request handlers on a dedicated
//! `lxmf.propagation.control` SINGLE destination, gated by an identity
//! allow list (`reference/LXMF/LXMF/LXMRouter.py:672-676`):
//!
//! - [`STATS_GET_PATH`] answers with the node-stats map
//!   (`compile_stats`, `reference/LXMF/LXMF/LXMRouter.py:769-836`),
//! - [`SYNC_REQUEST_PATH`] takes a 16-byte peer destination hash and
//!   triggers that peer's sync (`peer_sync_request`, `:843-853`),
//! - [`UNPEER_REQUEST_PATH`] takes the same and breaks the peering
//!   (`peer_unpeer_request`, `:855-865`).
//!
//! Payloads ride the request/response mechanism as plain MessagePack
//! values, the encoding `RNS.vendor.umsgpack` produces: the stats response
//! is a string-keyed map, a trigger's success is `true`, and every refusal
//! is one of `LXMPeer`'s integer error codes ([`PeerError`]). The client
//! half (`lxmd --status/--peers/--sync/--break --remote`,
//! `reference/LXMF/LXMF/Utilities/lxmd.py:649-680`) identifies on the
//! link and reads these back, so both sides of this codec face a genuine
//! Python peer.
//!
//! This module owns only the codec; the destination, the allow list and
//! the counters live in the daemon (`lnpnd`).

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::msgpack;
use crate::propagation::PeerError;

/// `LXMRouter.STATS_GET_PATH` (`reference/LXMF/LXMF/LXMRouter.py:89`).
pub const STATS_GET_PATH: &str = "/pn/get/stats";
/// `LXMRouter.SYNC_REQUEST_PATH` (`reference/LXMF/LXMF/LXMRouter.py:90`).
pub const SYNC_REQUEST_PATH: &str = "/pn/peer/sync";
/// `LXMRouter.UNPEER_REQUEST_PATH` (`reference/LXMF/LXMF/LXMRouter.py:91`).
pub const UNPEER_REQUEST_PATH: &str = "/pn/peer/unpeer";

/// Aspects of the control destination under [`crate::node::APP_NAME`]
/// (`reference/LXMF/LXMF/LXMRouter.py:673`).
pub const CONTROL_ASPECTS: [&str; 2] = ["propagation", "control"];

/// `RNS.Transport.PATHFINDER_M` — the hop count meaning "unknown", which
/// the reference reports for a peer it has no path to
/// (`compile_stats` fills `network_distance` from `Transport.hops_to`,
/// `reference/LXMF/LXMF/LXMRouter.py:793`; the sentinel is
/// `reference/Reticulum/RNS/Transport.py:97`).
pub const HOPS_UNKNOWN: u64 = 128;

/// One peer's entry in the stats response — the fields `compile_stats`
/// writes per peer (`reference/LXMF/LXMF/LXMRouter.py:775-803`), in its
/// insertion order.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlPeerStats {
    /// The map key: the peer's `lxmf.propagation` destination hash.
    pub peer_id: [u8; 16],
    /// `"static"` or `"discovered"` under the `type` key.
    pub is_static: bool,
    /// `LXMPeer` transport-state ladder value (`LXMPeer.py:17-22`).
    pub state: u64,
    pub alive: bool,
    pub name: Option<String>,
    pub last_heard: u64,
    pub next_sync_attempt: u64,
    pub last_sync_attempt: u64,
    pub sync_backoff: u64,
    pub peering_timebase: u64,
    /// Link-establishment rate, bits per second.
    pub ler: u64,
    /// Sync-transfer rate, bits per second (`str` in the reference map;
    /// renamed here because `str` is not a field name in Rust).
    pub str_rate: u64,
    pub transfer_limit_kb: Option<u64>,
    pub sync_limit_kb: Option<u64>,
    pub target_stamp_cost: Option<u64>,
    pub stamp_cost_flexibility: Option<u64>,
    pub peering_cost: Option<u64>,
    /// The mined peering key's measured value, `None` before mining
    /// (`peering_key_value`, `reference/LXMF/LXMF/LXMPeer.py:238-240`).
    pub peering_key_value: Option<u64>,
    /// Hops to the peer; [`HOPS_UNKNOWN`] without a path.
    pub network_distance: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub acceptance_rate: f64,
    /// The `messages` sub-map, in its order (`LXMRouter.py:797-802`).
    pub offered: u64,
    pub outgoing: u64,
    pub incoming: u64,
    pub unhandled: u64,
}

/// The stats response — the top-level map `compile_stats` returns
/// (`reference/LXMF/LXMF/LXMRouter.py:805-834`), in its insertion order.
///
/// The three transfer limits are kilobytes of 1000 bytes, the unit the
/// whole propagation protocol announces in; `lxmd`'s printer multiplies
/// by 1000 before pretty-printing (`lxmd.py:734`).
#[derive(Debug, Clone, PartialEq)]
pub struct ControlNodeStats {
    pub identity_hash: [u8; 16],
    pub destination_hash: [u8; 16],
    pub uptime_secs: f64,
    pub delivery_limit_kb: Option<u64>,
    pub propagation_limit_kb: Option<u64>,
    pub sync_limit_kb: Option<u64>,
    pub target_stamp_cost: u64,
    pub stamp_cost_flexibility: u64,
    pub peering_cost: u64,
    pub max_peering_cost: u64,
    pub autopeer_maxdepth: Option<u64>,
    pub from_static_only: bool,
    pub messagestore_count: u64,
    pub messagestore_bytes: u64,
    pub messagestore_limit_bytes: Option<u64>,
    pub client_propagation_messages_received: u64,
    pub client_propagation_messages_served: u64,
    pub unpeered_propagation_incoming: u64,
    pub unpeered_propagation_rx_bytes: u64,
    pub static_peers: u64,
    pub discovered_peers: u64,
    pub total_peers: u64,
    pub max_peers: Option<u64>,
    pub peers: Vec<ControlPeerStats>,
}

fn opt_uint(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => msgpack::uint(out, value),
        None => msgpack::nil(out),
    }
}

impl ControlPeerStats {
    fn encode_value(&self, out: &mut Vec<u8>) {
        msgpack::map(out, 22);
        msgpack::string(out, "type");
        msgpack::string(
            out,
            if self.is_static {
                "static"
            } else {
                "discovered"
            },
        );
        msgpack::string(out, "state");
        msgpack::uint(out, self.state);
        msgpack::string(out, "alive");
        msgpack::bool(out, self.alive);
        msgpack::string(out, "name");
        match &self.name {
            Some(name) => msgpack::string(out, name),
            None => msgpack::nil(out),
        }
        msgpack::string(out, "last_heard");
        msgpack::uint(out, self.last_heard);
        msgpack::string(out, "next_sync_attempt");
        msgpack::uint(out, self.next_sync_attempt);
        msgpack::string(out, "last_sync_attempt");
        msgpack::uint(out, self.last_sync_attempt);
        msgpack::string(out, "sync_backoff");
        msgpack::uint(out, self.sync_backoff);
        msgpack::string(out, "peering_timebase");
        msgpack::uint(out, self.peering_timebase);
        msgpack::string(out, "ler");
        msgpack::uint(out, self.ler);
        msgpack::string(out, "str");
        msgpack::uint(out, self.str_rate);
        msgpack::string(out, "transfer_limit");
        opt_uint(out, self.transfer_limit_kb);
        msgpack::string(out, "sync_limit");
        opt_uint(out, self.sync_limit_kb);
        msgpack::string(out, "target_stamp_cost");
        opt_uint(out, self.target_stamp_cost);
        msgpack::string(out, "stamp_cost_flexibility");
        opt_uint(out, self.stamp_cost_flexibility);
        msgpack::string(out, "peering_cost");
        opt_uint(out, self.peering_cost);
        msgpack::string(out, "peering_key");
        opt_uint(out, self.peering_key_value);
        msgpack::string(out, "network_distance");
        msgpack::uint(out, self.network_distance);
        msgpack::string(out, "rx_bytes");
        msgpack::uint(out, self.rx_bytes);
        msgpack::string(out, "tx_bytes");
        msgpack::uint(out, self.tx_bytes);
        msgpack::string(out, "acceptance_rate");
        msgpack::f64(out, self.acceptance_rate);
        msgpack::string(out, "messages");
        msgpack::map(out, 4);
        msgpack::string(out, "offered");
        msgpack::uint(out, self.offered);
        msgpack::string(out, "outgoing");
        msgpack::uint(out, self.outgoing);
        msgpack::string(out, "incoming");
        msgpack::uint(out, self.incoming);
        msgpack::string(out, "unhandled");
        msgpack::uint(out, self.unhandled);
    }

    fn default_for(peer_id: [u8; 16]) -> Self {
        Self {
            peer_id,
            is_static: false,
            state: 0,
            alive: false,
            name: None,
            last_heard: 0,
            next_sync_attempt: 0,
            last_sync_attempt: 0,
            sync_backoff: 0,
            peering_timebase: 0,
            ler: 0,
            str_rate: 0,
            transfer_limit_kb: None,
            sync_limit_kb: None,
            target_stamp_cost: None,
            stamp_cost_flexibility: None,
            peering_cost: None,
            peering_key_value: None,
            network_distance: HOPS_UNKNOWN,
            rx_bytes: 0,
            tx_bytes: 0,
            acceptance_rate: 0.0,
            offered: 0,
            outgoing: 0,
            incoming: 0,
            unhandled: 0,
        }
    }

    fn decode_value(
        peer_id: [u8; 16],
        bytes: &[u8],
        position: &mut usize,
    ) -> Result<Self, msgpack::Error> {
        let mut stats = Self::default_for(peer_id);
        let entries = msgpack::map_len(bytes, position)?;
        for _ in 0..entries {
            let key = msgpack::read_str(bytes, position)?;
            match key {
                "type" => stats.is_static = msgpack::read_str(bytes, position)? == "static",
                "state" => stats.state = read_uint_tolerant(bytes, position)?,
                "alive" => stats.alive = msgpack::read_bool(bytes, position)?,
                "name" => {
                    stats.name = match msgpack::peek_kind(bytes, *position)? {
                        msgpack::Kind::Nil => {
                            msgpack::read_nil(bytes, position)?;
                            None
                        }
                        msgpack::Kind::Str => Some(msgpack::read_str(bytes, position)?.to_string()),
                        // The reference may deliver a peer name as raw
                        // bytes when the announce carried non-UTF-8 data.
                        _ => String::from_utf8(msgpack::read_bin(bytes, position)?.to_vec()).ok(),
                    }
                }
                "last_heard" => stats.last_heard = read_uint_tolerant(bytes, position)?,
                "next_sync_attempt" => {
                    stats.next_sync_attempt = read_uint_tolerant(bytes, position)?
                }
                "last_sync_attempt" => {
                    stats.last_sync_attempt = read_uint_tolerant(bytes, position)?
                }
                "sync_backoff" => stats.sync_backoff = read_uint_tolerant(bytes, position)?,
                "peering_timebase" => stats.peering_timebase = read_uint_tolerant(bytes, position)?,
                "ler" => stats.ler = read_uint_tolerant(bytes, position)?,
                "str" => stats.str_rate = read_uint_tolerant(bytes, position)?,
                "transfer_limit" => stats.transfer_limit_kb = read_opt_uint(bytes, position)?,
                "sync_limit" => stats.sync_limit_kb = read_opt_uint(bytes, position)?,
                "target_stamp_cost" => stats.target_stamp_cost = read_opt_uint(bytes, position)?,
                "stamp_cost_flexibility" => {
                    stats.stamp_cost_flexibility = read_opt_uint(bytes, position)?
                }
                "peering_cost" => stats.peering_cost = read_opt_uint(bytes, position)?,
                "peering_key" => stats.peering_key_value = read_opt_uint(bytes, position)?,
                "network_distance" => stats.network_distance = read_uint_tolerant(bytes, position)?,
                "rx_bytes" => stats.rx_bytes = read_uint_tolerant(bytes, position)?,
                "tx_bytes" => stats.tx_bytes = read_uint_tolerant(bytes, position)?,
                "acceptance_rate" => {
                    stats.acceptance_rate = msgpack::read_number_f64(bytes, position)?
                }
                "messages" => {
                    let inner = msgpack::map_len(bytes, position)?;
                    for _ in 0..inner {
                        let inner_key = msgpack::read_str(bytes, position)?;
                        let value = read_uint_tolerant(bytes, position)?;
                        match inner_key {
                            "offered" => stats.offered = value,
                            "outgoing" => stats.outgoing = value,
                            "incoming" => stats.incoming = value,
                            "unhandled" => stats.unhandled = value,
                            _ => {}
                        }
                    }
                }
                _ => msgpack::skip(bytes, position)?,
            }
        }
        Ok(stats)
    }
}

/// Read a number that a Python sender may have packed as int or float
/// (e.g. `next_sync_attempt` is `time.time()`-derived), clamped at zero.
fn read_uint_tolerant(bytes: &[u8], position: &mut usize) -> Result<u64, msgpack::Error> {
    let value = msgpack::read_number_f64(bytes, position)?;
    if value.is_finite() && value > 0.0 {
        Ok(value as u64)
    } else {
        Ok(0)
    }
}

fn read_opt_uint(bytes: &[u8], position: &mut usize) -> Result<Option<u64>, msgpack::Error> {
    if msgpack::peek_kind(bytes, *position)? == msgpack::Kind::Nil {
        msgpack::read_nil(bytes, position)?;
        return Ok(None);
    }
    read_uint_tolerant(bytes, position).map(Some)
}

impl ControlNodeStats {
    /// Encode the stats response value, key for key in `compile_stats`'s
    /// insertion order (`reference/LXMF/LXMF/LXMRouter.py:805-834`) so the
    /// map a genuine `lxmd --status` unpacks holds every field its printer
    /// reads (`get_status`, `reference/LXMF/LXMF/Utilities/lxmd.py:698-758`).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        msgpack::map(&mut out, 21);
        msgpack::string(&mut out, "identity_hash");
        msgpack::bin(&mut out, &self.identity_hash);
        msgpack::string(&mut out, "destination_hash");
        msgpack::bin(&mut out, &self.destination_hash);
        msgpack::string(&mut out, "uptime");
        msgpack::f64(&mut out, self.uptime_secs);
        msgpack::string(&mut out, "delivery_limit");
        opt_uint(&mut out, self.delivery_limit_kb);
        msgpack::string(&mut out, "propagation_limit");
        opt_uint(&mut out, self.propagation_limit_kb);
        msgpack::string(&mut out, "sync_limit");
        opt_uint(&mut out, self.sync_limit_kb);
        msgpack::string(&mut out, "target_stamp_cost");
        msgpack::uint(&mut out, self.target_stamp_cost);
        msgpack::string(&mut out, "stamp_cost_flexibility");
        msgpack::uint(&mut out, self.stamp_cost_flexibility);
        msgpack::string(&mut out, "peering_cost");
        msgpack::uint(&mut out, self.peering_cost);
        msgpack::string(&mut out, "max_peering_cost");
        msgpack::uint(&mut out, self.max_peering_cost);
        msgpack::string(&mut out, "autopeer_maxdepth");
        opt_uint(&mut out, self.autopeer_maxdepth);
        msgpack::string(&mut out, "from_static_only");
        msgpack::bool(&mut out, self.from_static_only);
        msgpack::string(&mut out, "messagestore");
        msgpack::map(&mut out, 3);
        msgpack::string(&mut out, "count");
        msgpack::uint(&mut out, self.messagestore_count);
        msgpack::string(&mut out, "bytes");
        msgpack::uint(&mut out, self.messagestore_bytes);
        msgpack::string(&mut out, "limit");
        opt_uint(&mut out, self.messagestore_limit_bytes);
        msgpack::string(&mut out, "clients");
        msgpack::map(&mut out, 2);
        msgpack::string(&mut out, "client_propagation_messages_received");
        msgpack::uint(&mut out, self.client_propagation_messages_received);
        msgpack::string(&mut out, "client_propagation_messages_served");
        msgpack::uint(&mut out, self.client_propagation_messages_served);
        msgpack::string(&mut out, "unpeered_propagation_incoming");
        msgpack::uint(&mut out, self.unpeered_propagation_incoming);
        msgpack::string(&mut out, "unpeered_propagation_rx_bytes");
        msgpack::uint(&mut out, self.unpeered_propagation_rx_bytes);
        msgpack::string(&mut out, "static_peers");
        msgpack::uint(&mut out, self.static_peers);
        msgpack::string(&mut out, "discovered_peers");
        msgpack::uint(&mut out, self.discovered_peers);
        msgpack::string(&mut out, "total_peers");
        msgpack::uint(&mut out, self.total_peers);
        msgpack::string(&mut out, "max_peers");
        opt_uint(&mut out, self.max_peers);
        msgpack::string(&mut out, "peers");
        msgpack::map(&mut out, self.peers.len());
        for peer in &self.peers {
            msgpack::bin(&mut out, &peer.peer_id);
            peer.encode_value(&mut out);
        }
        out
    }

    /// Decode a stats response from either implementation. Unknown keys
    /// are skipped, missing ones keep zero defaults: the reference grows
    /// this map (it has before), and a client that hard-fails on a new
    /// field would break on the next upstream release.
    pub fn decode(bytes: &[u8]) -> Result<Self, msgpack::Error> {
        let mut position = 0;
        Self::decode_at(bytes, &mut position)
    }

    fn decode_at(bytes: &[u8], position: &mut usize) -> Result<Self, msgpack::Error> {
        let mut stats = Self {
            identity_hash: [0; 16],
            destination_hash: [0; 16],
            uptime_secs: 0.0,
            delivery_limit_kb: None,
            propagation_limit_kb: None,
            sync_limit_kb: None,
            target_stamp_cost: 0,
            stamp_cost_flexibility: 0,
            peering_cost: 0,
            max_peering_cost: 0,
            autopeer_maxdepth: None,
            from_static_only: false,
            messagestore_count: 0,
            messagestore_bytes: 0,
            messagestore_limit_bytes: None,
            client_propagation_messages_received: 0,
            client_propagation_messages_served: 0,
            unpeered_propagation_incoming: 0,
            unpeered_propagation_rx_bytes: 0,
            static_peers: 0,
            discovered_peers: 0,
            total_peers: 0,
            max_peers: None,
            peers: Vec::new(),
        };
        let entries = msgpack::map_len(bytes, position)?;
        for _ in 0..entries {
            let key = msgpack::read_str(bytes, position)?;
            match key {
                "identity_hash" => {
                    stats.identity_hash = msgpack::read_bin(bytes, position)?
                        .try_into()
                        .map_err(|_| msgpack::Error::Type)?
                }
                "destination_hash" => {
                    stats.destination_hash = msgpack::read_bin(bytes, position)?
                        .try_into()
                        .map_err(|_| msgpack::Error::Type)?
                }
                "uptime" => stats.uptime_secs = msgpack::read_number_f64(bytes, position)?,
                "delivery_limit" => stats.delivery_limit_kb = read_opt_uint(bytes, position)?,
                "propagation_limit" => stats.propagation_limit_kb = read_opt_uint(bytes, position)?,
                "sync_limit" => stats.sync_limit_kb = read_opt_uint(bytes, position)?,
                "target_stamp_cost" => {
                    stats.target_stamp_cost = read_uint_tolerant(bytes, position)?
                }
                "stamp_cost_flexibility" => {
                    stats.stamp_cost_flexibility = read_uint_tolerant(bytes, position)?
                }
                "peering_cost" => stats.peering_cost = read_uint_tolerant(bytes, position)?,
                "max_peering_cost" => stats.max_peering_cost = read_uint_tolerant(bytes, position)?,
                "autopeer_maxdepth" => stats.autopeer_maxdepth = read_opt_uint(bytes, position)?,
                "from_static_only" => stats.from_static_only = msgpack::read_bool(bytes, position)?,
                "messagestore" => {
                    let inner = msgpack::map_len(bytes, position)?;
                    for _ in 0..inner {
                        let inner_key = msgpack::read_str(bytes, position)?;
                        match inner_key {
                            "count" => {
                                stats.messagestore_count = read_uint_tolerant(bytes, position)?
                            }
                            "bytes" => {
                                stats.messagestore_bytes = read_uint_tolerant(bytes, position)?
                            }
                            "limit" => {
                                stats.messagestore_limit_bytes = read_opt_uint(bytes, position)?
                            }
                            _ => msgpack::skip(bytes, position)?,
                        }
                    }
                }
                "clients" => {
                    let inner = msgpack::map_len(bytes, position)?;
                    for _ in 0..inner {
                        let inner_key = msgpack::read_str(bytes, position)?;
                        match inner_key {
                            "client_propagation_messages_received" => {
                                stats.client_propagation_messages_received =
                                    read_uint_tolerant(bytes, position)?
                            }
                            "client_propagation_messages_served" => {
                                stats.client_propagation_messages_served =
                                    read_uint_tolerant(bytes, position)?
                            }
                            _ => msgpack::skip(bytes, position)?,
                        }
                    }
                }
                "unpeered_propagation_incoming" => {
                    stats.unpeered_propagation_incoming = read_uint_tolerant(bytes, position)?
                }
                "unpeered_propagation_rx_bytes" => {
                    stats.unpeered_propagation_rx_bytes = read_uint_tolerant(bytes, position)?
                }
                "static_peers" => stats.static_peers = read_uint_tolerant(bytes, position)?,
                "discovered_peers" => stats.discovered_peers = read_uint_tolerant(bytes, position)?,
                "total_peers" => stats.total_peers = read_uint_tolerant(bytes, position)?,
                "max_peers" => stats.max_peers = read_opt_uint(bytes, position)?,
                "peers" => {
                    let count = msgpack::map_len(bytes, position)?;
                    for _ in 0..count {
                        let peer_id: [u8; 16] = msgpack::read_bin(bytes, position)?
                            .try_into()
                            .map_err(|_| msgpack::Error::Type)?;
                        stats
                            .peers
                            .push(ControlPeerStats::decode_value(peer_id, bytes, position)?);
                    }
                }
                _ => msgpack::skip(bytes, position)?,
            }
        }
        Ok(stats)
    }
}

/// A decoded control response, any of the three paths.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponse {
    /// The stats map ([`STATS_GET_PATH`]).
    Stats(Box<ControlNodeStats>),
    /// `True` — a sync or unpeer trigger was accepted
    /// (`reference/LXMF/LXMF/LXMRouter.py:853`, `:865`).
    Ack,
    /// One of `LXMPeer`'s error codes.
    Error(PeerError),
    /// msgpack nil — the reference answers `None` for a request it could
    /// not process at all.
    Nil,
}

impl ControlResponse {
    pub fn decode(bytes: &[u8]) -> Result<Self, msgpack::Error> {
        let mut position = 0;
        match msgpack::peek_kind(bytes, position)? {
            msgpack::Kind::Map => Ok(Self::Stats(Box::new(ControlNodeStats::decode_at(
                bytes,
                &mut position,
            )?))),
            msgpack::Kind::True | msgpack::Kind::False => {
                msgpack::read_bool(bytes, &mut position)?;
                Ok(Self::Ack)
            }
            msgpack::Kind::Nil => Ok(Self::Nil),
            _ => {
                let code = msgpack::read_uint(bytes, &mut position)?;
                PeerError::try_from(code)
                    .map(Self::Error)
                    .map_err(|_| msgpack::Error::Type)
            }
        }
    }
}

/// Encode a control error response: the bare `LXMPeer` error code as a
/// MessagePack integer, what `umsgpack.packb(0xf1)` produces.
pub fn encode_control_error(error: PeerError) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::uint(&mut out, error.code() as u64);
    out
}

/// Encode a trigger acknowledgement: MessagePack `true`.
pub fn encode_control_ack() -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::bool(&mut out, true);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ControlNodeStats {
        ControlNodeStats {
            identity_hash: [0x11; 16],
            destination_hash: [0x22; 16],
            uptime_secs: 42.5,
            delivery_limit_kb: Some(1000),
            propagation_limit_kb: Some(256),
            sync_limit_kb: Some(10240),
            target_stamp_cost: 16,
            stamp_cost_flexibility: 3,
            peering_cost: 18,
            max_peering_cost: 26,
            autopeer_maxdepth: Some(4),
            from_static_only: false,
            messagestore_count: 2,
            messagestore_bytes: 4096,
            messagestore_limit_bytes: Some(500_000_000),
            client_propagation_messages_received: 5,
            client_propagation_messages_served: 3,
            unpeered_propagation_incoming: 1,
            unpeered_propagation_rx_bytes: 2048,
            static_peers: 1,
            discovered_peers: 1,
            total_peers: 2,
            max_peers: Some(20),
            peers: alloc::vec![
                ControlPeerStats {
                    peer_id: [0x33; 16],
                    is_static: true,
                    name: Some("pn-b".into()),
                    alive: true,
                    state: 0,
                    last_heard: 1000,
                    next_sync_attempt: 0,
                    last_sync_attempt: 900,
                    sync_backoff: 0,
                    peering_timebase: 800,
                    ler: 1200,
                    str_rate: 40_000,
                    transfer_limit_kb: Some(256),
                    sync_limit_kb: Some(10240),
                    target_stamp_cost: Some(16),
                    stamp_cost_flexibility: Some(3),
                    peering_cost: Some(18),
                    peering_key_value: Some(19),
                    network_distance: 1,
                    rx_bytes: 100,
                    tx_bytes: 200,
                    acceptance_rate: 1.0,
                    offered: 4,
                    outgoing: 4,
                    incoming: 2,
                    unhandled: 0,
                },
                ControlPeerStats::default_for([0x44; 16]),
            ],
        }
    }

    #[test]
    fn stats_round_trip() {
        let stats = sample();
        let encoded = stats.encode();
        let decoded = ControlNodeStats::decode(&encoded).expect("decode");
        assert_eq!(decoded, stats);
    }

    #[test]
    fn control_responses_decode() {
        assert_eq!(
            ControlResponse::decode(&encode_control_ack()),
            Ok(ControlResponse::Ack)
        );
        assert_eq!(
            ControlResponse::decode(&encode_control_error(PeerError::NoAccess)),
            Ok(ControlResponse::Error(PeerError::NoAccess))
        );
        assert_eq!(ControlResponse::decode(&[0xC0]), Ok(ControlResponse::Nil));
        let stats = sample();
        match ControlResponse::decode(&stats.encode()) {
            Ok(ControlResponse::Stats(decoded)) => assert_eq!(*decoded, stats),
            other => panic!("stats response must decode as stats: {other:?}"),
        }
    }
}
