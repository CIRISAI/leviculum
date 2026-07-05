//! Remote-management `/status` responder — `rnstatus -R` server (Codeberg #86).
//!
//! The server half of the remote transport-instance status flow. The core
//! creates the `rnstransport.remote.management` IN/SINGLE destination on the
//! transport identity and registers a `/status` request handler gated by an
//! allow-list (`NodeCore::enable_remote_management`); after the core has
//! verified the destination and the allow-list, it emits
//! a `NodeEvent::RequestReceived`. This responder turns that event into the
//! same stats bundle the local RPC path serves and replies over the link.
//!
//! Mirrors Python `Transport.remote_status_handler` (`Transport.py:2814`):
//! the response is `[get_interface_stats()]`, plus `get_link_count()` when the
//! request's first element is `True` (the `-l` flag). Small bundles go back as
//! one RESPONSE packet; a real interface-stats bundle exceeds the link MDU and
//! is delivered as a response Resource, exactly as Python does.
//!
//! This lives in the driver (daemon/transport policy), not the interface
//! layer: the flow is media-agnostic. The stats bundle is built by the same
//! `rpc::handlers::build_interface_stats` the local `interface_stats` RPC uses,
//! so a remote `rnstatus -R` and a local `rnstatus` see identical data.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use leviculum_core::constants::TRUNCATED_HASHBYTES;
use leviculum_core::link::LinkId;
use leviculum_core::{RequestError, TickOutput};
use serde_pickle::value::Value;

use super::StdNodeCore;
use crate::interfaces::{InterfaceOnlineMap, InterfaceStatsMap};

/// The request path served by the remote-management responder.
const STATUS_PATH: &str = "/status";

/// Serves `/status` requests on the remote-management destination.
///
/// Constructed in `ReticulumNode::start()` when the core reports a
/// remote-management destination (i.e. remote management is enabled) and
/// consulted from the event loop's `dispatch_output` for every
/// `RequestReceived` event.
pub(crate) struct RemoteMgmtResponder {
    /// Shared interface I/O counters (same map the RPC server reads).
    iface_stats_map: InterfaceStatsMap,
    /// Per-interface online status (same map the RPC server reads).
    iface_online_map: InterfaceOnlineMap,
    /// Node start time, for the `transport_uptime` field.
    start_time: Instant,
    /// Aggregated AutoInterface peer count across all sections, for the
    /// `peers` field (0 when no AutoInterface is configured).
    auto_peer_count: super::AutoPeerCount,
}

impl RemoteMgmtResponder {
    pub(crate) fn new(
        iface_stats_map: InterfaceStatsMap,
        iface_online_map: InterfaceOnlineMap,
        start_time: Instant,
        auto_peer_count: super::AutoPeerCount,
    ) -> Self {
        Self {
            iface_stats_map,
            iface_online_map,
            start_time,
            auto_peer_count,
        }
    }

    /// Handle a `RequestReceived` event. Returns the [`TickOutput`] produced by
    /// sending the response (to be dispatched by the caller), or `None` if the
    /// request is not for `/status` or the response could not be sent.
    ///
    /// The core has already checked the destination and the allow-list before
    /// emitting the event, so a request that reaches here is authorised.
    pub(crate) fn handle_request(
        &self,
        inner: &Arc<Mutex<StdNodeCore>>,
        link_id: &LinkId,
        request_id: &[u8; TRUNCATED_HASHBYTES],
        path: &str,
        data: &[u8],
    ) -> Option<TickOutput> {
        if path != STATUS_PATH {
            return None;
        }

        let include_lstats = decode_include_lstats(data);
        let auto_peer_count = self.auto_peer_count.total();

        let mut core = inner.lock().unwrap();

        // Build the same bundle the local `interface_stats` RPC serves, then
        // append the link count when requested (Python appends
        // get_link_count() only when data[0] == True).
        let stats = crate::rpc::handlers::build_interface_stats(
            &mut core,
            self.start_time,
            &self.iface_stats_map,
            &self.iface_online_map,
            auto_peer_count,
        );
        let mut items = vec![stats];
        if include_lstats {
            items.push(Value::I64(core.active_link_count() as i64));
        }
        let response_value = Value::List(items);

        // Encode the response list as a single msgpack value (umsgpack.packb).
        let response_bytes = match crate::rpc::pickle::value_to_rmpv(&response_value) {
            Ok(mv) => {
                let mut buf = Vec::new();
                if let Err(e) = rmpv::encode::write_value(&mut buf, &mv) {
                    tracing::warn!("remote status: failed to encode response: {e}");
                    return None;
                }
                buf
            }
            Err(e) => {
                tracing::warn!("remote status: failed to build response value: {e}");
                return None;
            }
        };

        // A small bundle fits one RESPONSE packet; a real bundle exceeds the
        // link MDU and is delivered as a response Resource (Python parity).
        match core.send_response(link_id, request_id, &response_bytes) {
            Ok(output) => Some(output),
            Err(RequestError::PayloadTooLarge) => {
                match core.send_response_resource(link_id, request_id, &response_bytes) {
                    Ok((_, output)) => Some(output),
                    Err(e) => {
                        tracing::warn!("remote status: failed to send response resource: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                tracing::warn!("remote status: failed to send response: {e}");
                None
            }
        }
    }
}

/// Decode the request payload `[include_lstats]` and return whether link stats
/// were requested. Mirrors Python `if isinstance(data, list) and len(data) > 0
/// and data[0] == True`: anything else (nil, empty, non-bool) is `false`.
fn decode_include_lstats(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    let mut cursor = std::io::Cursor::new(data);
    match rmpv::decode::read_value(&mut cursor) {
        Ok(rmpv::Value::Array(items)) => items.first().and_then(|v| v.as_bool()).unwrap_or(false),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_include_lstats_true_only_for_leading_true() {
        // [true] -> true
        assert!(decode_include_lstats(&[0x91, 0xc3]));
        // [false] -> false
        assert!(!decode_include_lstats(&[0x91, 0xc2]));
        // empty -> false
        assert!(!decode_include_lstats(&[]));
        // nil -> false
        assert!(!decode_include_lstats(&[0xc0]));
    }
}
