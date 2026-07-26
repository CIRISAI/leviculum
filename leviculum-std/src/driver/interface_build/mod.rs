//! Per-entry interface construction, shared by [`initialize_interfaces`] at
//! startup and [`spawn_interface`] at runtime so a type is wired once for both.
//! Each interface family lives in its own submodule; this module holds the
//! shared context and the type dispatch.
//!
//! [`initialize_interfaces`]: super::ReticulumNode
//! [`spawn_interface`]: super::ReticulumNode::spawn_interface

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::InterfaceHandle;
use leviculum_core::transport::InterfaceId;

use super::AutoPeerCount;

mod auto;
mod i2p;
mod kiss;
mod pipe;
mod rnode;
mod rnode_multi;
mod serial;
mod tcp;
mod udp;

/// Shared wiring the per-type builders need, so they do not depend on the
/// driver struct itself.
pub(super) struct InterfaceBuildCtx<'a> {
    pub next_id: &'a Arc<AtomicUsize>,
    pub new_iface_tx: &'a mpsc::Sender<InterfaceHandle>,
    pub reconnect_tx: &'a mpsc::Sender<InterfaceId>,
    pub tunnel_notify_tx: &'a mpsc::Sender<InterfaceId>,
    /// Test-only frame-corruption cadence.
    pub corrupt_every: Option<u64>,
    /// Storage root; I2P persists its per-interface keyfile under it.
    pub storage_path: Option<PathBuf>,
}

/// Outcome of building one configured interface section.
pub(super) enum Built {
    /// Handles the caller registers itself (config init) or dispatches through
    /// `new_iface_tx` (runtime attach).
    Handles(Vec<InterfaceHandle>),
    /// A listener/orchestrator that registered its own children through
    /// `new_iface_tx`, or an unknown type; the caller registers nothing.
    SelfManaged,
}

/// Construct the interface(s) for one section.
///
/// `idx` is the base [`InterfaceId`]: leaf interfaces take it directly (stable
/// across restarts at startup), fan-out children draw further ids from
/// `ctx.next_id`.
pub(super) fn build_interface(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
    auto_peer_count: &AutoPeerCount,
) -> Result<Built, Error> {
    match config.interface_type.as_str() {
        "TCPClientInterface" => tcp::build_client(idx, config, ctx),
        "TCPServerInterface" => tcp::build_server(config, ctx),
        "UDPInterface" => udp::build(idx, config, ctx),
        "AutoInterface" => auto::build(config, ctx, auto_peer_count),
        "RNodeInterface" => rnode::build(idx, config, ctx),
        "RNodeMultiInterface" => rnode_multi::build(idx, config, ctx),
        "SerialInterface" => serial::build(idx, config, ctx),
        "PipeInterface" => pipe::build(idx, config, ctx),
        "KISSInterface" | "AX25KISSInterface" => kiss::build(idx, config, ctx),
        "I2PInterface" => i2p::build(idx, config, ctx),
        other => {
            tracing::warn!("Unknown interface type: {}", other);
            Ok(Built::SelfManaged)
        }
    }
}
