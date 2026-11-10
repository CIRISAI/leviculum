//! The board's `/status` responder — `rnstatus -R <board>` (Codeberg #235).
//!
//! The core already does the front half: it registers
//! `rnstransport.remote.management`, gates `/status` behind the USB-loaded
//! allow-list, and raises `NodeEvent::RequestReceived` for a request that
//! passed both (`leviculum-core/src/node/mod.rs:794`). Until this module
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
//! encoding, tested byte-for-byte against the reference packer, and
//! [`leviculum_core::node::remote_status`] owns which event is a `/status`
//! request (exact management destination and path, decided before any row is
//! built) and how the answer leaves (a packet, or a response Resource only
//! when the packet refuses the size). This module only fills in the board's
//! three rows. When `leviculum-std`'s builder is next touched, that is the
//! seam to move it onto.

use alloc::vec::Vec;

use leviculum_core::node::remote_status::{send_status_response, status_request, StatusSendError};
use leviculum_core::node::{NodeCore, NodeEvent};
use leviculum_core::status_bundle::{encode_status_response, InterfaceStatus, StatusBundle};
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::transport::TickOutput;
use rand_core::CryptoRngCore;

use leviculum_firmware::iface_bytes::{self, names, IFACE_COUNT};

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
        let Some(request) = status_request(event, node.remote_mgmt_dest_hash()) else {
            continue;
        };
        // The core checked the allow-list before raising the event, so a
        // request that reaches here is from an identity the operator loaded
        // over USB.
        let include_lstats = request.include_link_stats;
        let response = build_response(node, include_lstats);
        crate::log::log_fmt(
            "[REMOTE_STATUS] ",
            format_args!("answered len={} lstats={}", response.len(), include_lstats),
        );
        // The two failure lines are the ones the board printed before the
        // send moved into core. A resource error is named by the kebab-case
        // word `PN_REJECT` already prints it with, not by its derived
        // `Debug`: that impl, its variant names and the formatter's padding
        // adapter it was the last user of were 1064 B of the RAK4631's flash
        // for this one line (order 1620).
        match send_status_response(node, &request, &response) {
            Ok(send) => out.merge(send),
            Err(StatusSendError::Resource(e)) => crate::log::log_fmt(
                "[REMOTE_STATUS] ",
                format_args!(
                    "resource failed: {}",
                    leviculum_pn_store::resource_error_name(&e)
                ),
            ),
            Err(StatusSendError::Packet(e)) => {
                crate::log::log_fmt("[REMOTE_STATUS] ", format_args!("send failed: {:?}", e))
            }
        }
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
        let traffic = iface_bytes::snapshot(id);
        InterfaceStatus {
            name: names()[id],
            // The live mirror the main loop keeps (`set_interface_online`),
            // which is the same bool the core routes on: a status line that
            // said "Up" for a carrier the core refuses to route over would
            // be the exact misreport `rnstatus -R` exists to prevent.
            status: node.interface_online(id),
            mode: node.interface_mode(id).as_u8(),
            // Only BLE has a notion of connected clients: the live link
            // count the main loop mirrors for path selection.
            clients: (id == iface_bytes::BLE).then(|| node.interface_peer_count(id) as u64),
            rxb: traffic.rxb,
            txb: traffic.txb,
            rxs: traffic.rxs,
            txs: traffic.txs,
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
    let link_count = include_lstats.then(|| node.transport_link_count() as u64);
    encode_status_response(&bundle, link_count)
}
