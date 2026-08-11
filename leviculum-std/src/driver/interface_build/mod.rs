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
    /// Applied to each TCP client's connect socket before it dials.
    pub outbound_socket_hook: Option<crate::socket_hook::OutboundSocketHook>,
    /// Reporting-side interface inventory (Codeberg #177): listeners register
    /// themselves here because they never become routable interfaces.
    pub inventory: crate::interfaces::inventory::SharedInventory,
    /// Whether transport is enabled, which decides the announce-rate defaults
    /// a listener reports (Reticulum.py:830-833).
    pub transport_enabled: bool,
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
        "TCPServerInterface" => tcp::build_server(idx, config, ctx),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Owned wiring a test `InterfaceBuildCtx` borrows from.
    struct CtxOwner {
        next_id: Arc<AtomicUsize>,
        new_iface_tx: mpsc::Sender<InterfaceHandle>,
        reconnect_tx: mpsc::Sender<InterfaceId>,
        tunnel_notify_tx: mpsc::Sender<InterfaceId>,
        inventory: crate::interfaces::inventory::SharedInventory,
    }

    impl CtxOwner {
        fn new() -> Self {
            let (new_iface_tx, _new_iface_rx) = mpsc::channel(4);
            let (reconnect_tx, _reconnect_rx) = mpsc::channel(4);
            let (tunnel_notify_tx, _tunnel_notify_rx) = mpsc::channel(4);
            Self {
                next_id: Arc::new(AtomicUsize::new(100)),
                new_iface_tx,
                reconnect_tx,
                tunnel_notify_tx,
                inventory: crate::interfaces::inventory::InterfaceInventory::shared(),
            }
        }

        fn ctx(&self) -> InterfaceBuildCtx<'_> {
            InterfaceBuildCtx {
                next_id: &self.next_id,
                new_iface_tx: &self.new_iface_tx,
                reconnect_tx: &self.reconnect_tx,
                tunnel_notify_tx: &self.tunnel_notify_tx,
                corrupt_every: None,
                storage_path: None,
                outbound_socket_hook: None,
                inventory: self.inventory.clone(),
                transport_enabled: false,
            }
        }
    }

    fn rnode_config(frequency: u64) -> InterfaceConfig {
        InterfaceConfig {
            interface_type: "RNodeInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            frequency: Some(frequency),
            bandwidth: Some(125_000),
            spreading_factor: Some(7),
            coding_rate: Some(5),
            ..Default::default()
        }
    }

    /// A 125 kHz carrier centred at 868.65 MHz sits in the 868.6-868.7 MHz
    /// alarm band (<= 25 kHz channel spacing only): interface build must
    /// refuse with a config error naming the band, not fall through to a
    /// "no known limit" default. The check fires before any port is opened.
    #[test]
    fn rnode_build_refuses_a_carrier_in_an_alarm_band() {
        let owner = CtxOwner::new();
        let err = rnode::build(0, &rnode_config(868_650_000), &owner.ctx())
            .err()
            .expect("868.65 MHz / 125 kHz must not build");
        let msg = err.to_string();
        assert!(msg.contains("868.6-868.7 MHz"), "names the band: {msg}");
    }

    /// The same block one sub-band over builds fine — the refusal is the gap,
    /// not the neighbourhood. Needs a runtime because a successful build
    /// spawns the interface tasks (the port itself may fail later; the
    /// reconnect loop owns that).
    #[tokio::test]
    async fn rnode_build_accepts_a_carrier_in_a_listed_sub_band() {
        let owner = CtxOwner::new();
        assert!(rnode::build(0, &rnode_config(869_525_000), &owner.ctx()).is_ok());
    }

    /// The `SerialInterface` LNode path refuses the same carrier the same
    /// way, before `serial_radio_config` resolves anything.
    #[test]
    fn serial_build_refuses_a_carrier_in_an_alarm_band() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "SerialInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            frequency: Some(868_650_000),
            ..Default::default()
        };
        let err = serial::build(0, &config, &owner.ctx())
            .err()
            .expect("868.65 MHz / 125 kHz must not build");
        let msg = err.to_string();
        assert!(msg.contains("868.6-868.7 MHz"), "names the band: {msg}");
    }

    /// The multi builder checks each subinterface's own frequency.
    #[test]
    fn rnode_multi_build_refuses_a_subinterface_in_an_alarm_band() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "RNodeMultiInterface".to_string(),
            port: Some("/dev/nonexistent-test-port".to_string()),
            subinterfaces: vec![crate::config::SubinterfaceConfig {
                name: "gap".to_string(),
                vport: Some(0),
                frequency: Some(868_650_000),
                bandwidth: Some(125_000),
                spreading_factor: Some(7),
                coding_rate: Some(5),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = rnode_multi::build(0, &config, &owner.ctx())
            .err()
            .expect("868.65 MHz / 125 kHz must not build");
        let msg = err.to_string();
        assert!(msg.contains("868.6-868.7 MHz"), "names the band: {msg}");
    }
}
