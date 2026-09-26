//! The board's `/status` responder — `rnstatus -R <board>` (Codeberg #235).
//!
//! The core already does the front half: it registers
//! `rnstransport.remote.management`, gates `/status` behind the USB-loaded
//! allow-list, and raises `NodeEvent::RequestReceived` for a request that
//! passed both (`leviculum-core/src/node/mod.rs:790`). Until this module
//! nothing on the board answered that event, so a query got path, link,
//! accepted identify — and then a timeout.
//!
//! # Why not the host responder
//!
//! `leviculum-std`'s `driver::remote_mgmt::RemoteMgmtResponder` answers the
//! same event on a PC, and the instruction for this batch was to share it if
//! sharing was possible. It is not, and the reason is not stylistic: that
//! responder's bundle comes from `rpc::handlers::build_interface_stats`, which
//! walks a `SharedInventory` of listeners and their spawned children, reads a
//! `Mutex`-guarded `InterfaceStatsMap` of `std` atomics, clones IFAC configs
//! into a `BTreeMap`, and returns a `serde_pickle::Value`. Every one of those
//! is `std`, and the world it describes — listeners, spawned client
//! connections, local IPC peers — does not exist on a board with three fixed
//! interfaces.
//!
//! What IS shared is the part that decides the wire format:
//! [`leviculum_core::status_bundle`] owns the key set, the key order and the
//! encoding, tested byte-for-byte against the reference packer. This module
//! only fills in the board's three rows. When `leviculum-std`'s builder is
//! next touched, that is the seam to move it onto.

use alloc::vec::Vec;

use leviculum_core::node::{NodeCore, NodeEvent};
use leviculum_core::status_bundle::{
    encode_status_response, request_wants_link_stats, InterfaceStatus, StatusBundle,
};
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::transport::TickOutput;
use leviculum_core::{LinkId, RequestError};
use rand_core::CryptoRngCore;

use crate::iface_bytes::{self, IFACE_COUNT, NAMES};

/// The request path served, as Python registers it
/// (`Transport.py:253-259`).
const STATUS_PATH: &str = "/status";

/// Answer every `/status` request in `events`, merging the sends into `out`.
///
/// Called from each binary's main loop wherever it already hands
/// `output.events` around. Cheap on the common path: one string compare per
/// request event, and no request events at all in ordinary traffic.
pub fn handle_events<R, C, S>(
    node: &mut NodeCore<R, C, S>,
    events: &[NodeEvent],
    out: &mut TickOutput,
) where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    for event in events {
        let NodeEvent::RequestReceived {
            link_id,
            destination_hash,
            request_id,
            path,
            data,
            ..
        } = event
        else {
            continue;
        };
        if path != STATUS_PATH || node.remote_mgmt_dest_hash() != Some(destination_hash) {
            continue;
        }
        // The core checked the allow-list before raising the event, so a
        // request that reaches here is from an identity the operator loaded
        // over USB.
        let include_lstats = request_wants_link_stats(data);
        let response = build_response(node, include_lstats);
        crate::log::log_fmt(
            "[REMOTE_STATUS] ",
            format_args!("answered len={} lstats={}", response.len(), include_lstats),
        );
        respond(node, link_id, request_id, &response, out);
    }
}

/// Assemble and encode the answer: the board's three interface rows, its
/// transport identity and its uptime, plus the link count when asked.
fn build_response<R, C, S>(node: &NodeCore<R, C, S>, include_lstats: bool) -> Vec<u8>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    // The LoRa row's `bitrate` is the nominal on-air rate of the PHY the
    // radio is actually running, by the reference's own formula
    // (`RNodeInterface.updateBitrate`, mirrored in
    // `leviculum_core::rnode::compute_bitrate`). The other two carriers have
    // no figure this board measures, and Python emits `None` for exactly that
    // case (`Reticulum.py:1425`), so the reference tool simply omits their
    // "Rate" line rather than being told a number nobody counted.
    let lora_bitrate = crate::lora::running_config().map(|phy| {
        u64::from(leviculum_core::rnode::compute_bitrate(
            phy.sf,
            phy.cr,
            phy.bandwidth_hz,
        ))
    });

    let mut rows: [InterfaceStatus; IFACE_COUNT] = core::array::from_fn(|id| {
        let (rxb, txb) = iface_bytes::counters(id).map_or((0, 0), |c| c.totals());
        let (rxs, txs) = iface_bytes::counters(id).map_or((0.0, 0.0), |c| c.speeds());
        InterfaceStatus {
            name: NAMES[id],
            // The live mirror the main loop keeps (`set_interface_online`),
            // which is the same bool the core routes on: a status line that
            // said "Up" for a carrier the core refuses to route over would
            // be the exact misreport `rnstatus -R` exists to prevent.
            status: node.interface_online(id),
            mode: node.interface_mode(id).as_u8(),
            // Only BLE has a notion of connected clients: the live link
            // count the main loop mirrors for path selection.
            clients: (id == iface_bytes::BLE).then(|| node.interface_peer_count(id) as u64),
            rxb,
            txb,
            rxs,
            txs,
            bitrate: None,
        }
    });
    rows[iface_bytes::LORA].bitrate = lora_bitrate;

    let transport_id = *node.identity().hash();
    let bundle = StatusBundle {
        interfaces: &rows,
        transport_id: Some(&transport_id),
        transport_uptime: embassy_time::Instant::now().as_millis() as f64 / 1000.0,
    };
    let link_count = include_lstats.then(|| node.transport_link_table_entries().len() as u64);
    encode_status_response(&bundle, link_count)
}

/// Send `response`, falling back to a response Resource when it does not fit
/// one packet. Same shape as the propagation engine's `respond`
/// (`pn.rs:3175`), which is the board's precedent for answering a request.
fn respond<R, C, S>(
    node: &mut NodeCore<R, C, S>,
    link_id: &LinkId,
    request_id: &[u8; 16],
    response: &[u8],
    out: &mut TickOutput,
) where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    match node.send_response(link_id, request_id, response) {
        Ok(send) => out.merge(send),
        Err(RequestError::PayloadTooLarge) => {
            match node.send_response_resource(link_id, request_id, response) {
                Ok((_, send)) => out.merge(send),
                Err(e) => crate::log::log_fmt(
                    "[REMOTE_STATUS] ",
                    format_args!("resource failed: {:?}", e),
                ),
            }
        }
        Err(e) => crate::log::log_fmt("[REMOTE_STATUS] ", format_args!("send failed: {:?}", e)),
    }
}
