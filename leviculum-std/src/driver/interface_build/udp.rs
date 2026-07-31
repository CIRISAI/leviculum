//! UDP interface builder.

use std::net::SocketAddr;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::udp::spawn_udp_interface;
use leviculum_core::transport::InterfaceId;

use super::{Built, InterfaceBuildCtx};

pub(super) fn build(
    idx: usize,
    config: &InterfaceConfig,
    _ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    // A configured `device` supplies the NIC's IPv4 broadcast address for
    // whichever of listen_ip / forward_ip is left unset (Codeberg #3,
    // UDPInterface.py:82-86). Explicit keys win over it.
    let device_broadcast = match config.device.as_deref() {
        Some(device) => Some(
            crate::interfaces::netdevice::resolve_if_broadcast(device)
                .map_err(|e| Error::Config(format!("UDPInterface device \"{}\": {}", device, e)))?
                .to_string(),
        ),
        None => None,
    };

    let listen_ip = config
        .listen_ip
        .as_deref()
        .or(device_broadcast.as_deref())
        .unwrap_or("0.0.0.0");
    let listen_port = config
        .listen_port
        .ok_or_else(|| Error::Config("UDPInterface requires listen_port".to_string()))?;
    let forward_ip = config
        .forward_ip
        .as_deref()
        .or(device_broadcast.as_deref())
        .ok_or_else(|| Error::Config("UDPInterface requires forward_ip".to_string()))?;

    let listen_addr: SocketAddr = format!("{}:{}", listen_ip, listen_port)
        .parse()
        .map_err(|e| Error::Config(format!("UDPInterface invalid listen address: {}", e)))?;
    // `forward_ip` may hold several comma-separated addresses (Rust-only
    // extension); each outgoing datagram goes to every one of them.
    let forward_addrs =
        crate::interfaces::udp::parse_forward_addrs(forward_ip, config.forward_port).map_err(
            |e| match e {
                crate::interfaces::udp::ForwardAddrError::MissingPort => {
                    Error::Config("UDPInterface requires forward_port".to_string())
                }
                crate::interfaces::udp::ForwardAddrError::Invalid(msg) => {
                    Error::Config(format!("UDPInterface invalid forward address: {}", msg))
                }
            },
        )?;

    let iface_name = format!("udp_{}", idx);
    let id = InterfaceId(idx);
    let forward_desc = forward_addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let handle = spawn_udp_interface(id, iface_name, listen_addr, forward_addrs)?;
    tracing::info!(
        "UDP interface listening on {}, forwarding to {}",
        listen_addr,
        forward_desc
    );
    Ok(Built::Handles(vec![handle]))
}
