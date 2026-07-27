//! TCP client and server interface builders.

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::tcp::{
    spawn_tcp_client_with_reconnect, spawn_tcp_server, TcpClientConfig,
    DEFAULT_RECONNECT_MAX_INTERVAL, DEFAULT_TCP_CONNECT_TIMEOUT, TCP_DEFAULT_BUFFER_SIZE,
};
use leviculum_core::transport::InterfaceId;

use super::super::build_ifac_config;
use super::{Built, InterfaceBuildCtx};

pub(super) fn build_client(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    let target_host = config
        .target_host
        .as_ref()
        .ok_or_else(|| Error::Config("TCPClientInterface requires target_host".to_string()))?;
    let target_port = config
        .target_port
        .ok_or_else(|| Error::Config("TCPClientInterface requires target_port".to_string()))?;

    let addr_str = format!("{}:{}", target_host, target_port);
    let addr: SocketAddr = addr_str
        .as_str()
        .to_socket_addrs()
        .map_err(|e| Error::Config(format!("cannot resolve {}: {}", addr_str, e)))?
        .next()
        .ok_or_else(|| Error::Config(format!("no addresses for {}", addr_str)))?;

    let iface_name = format!("tcp_client_{}", idx);
    let id = InterfaceId(idx);
    let buffer_size = config.buffer_size.unwrap_or(TCP_DEFAULT_BUFFER_SIZE);
    let reconnect_interval = Duration::from_secs(config.reconnect_interval_secs.unwrap_or(5));

    // TCP interfaces don't register a bitrate cap (bitrate=0 means unlimited).
    // Future LoRa/serial interfaces should call register_interface_bitrate(id,
    // bitrate) after registration to enable per-interface announce caps.
    let handle = spawn_tcp_client_with_reconnect(TcpClientConfig {
        id,
        name: iface_name,
        addr,
        buffer_size,
        corrupt_every: ctx.corrupt_every,
        reconnect_interval,
        max_reconnect_tries: config.max_reconnect_tries,
        reconnect_max_interval: DEFAULT_RECONNECT_MAX_INTERVAL,
        connect_timeout: DEFAULT_TCP_CONNECT_TIMEOUT,
        reconnect_notify: Some(ctx.reconnect_tx.clone()),
        // Tunnel-capable: a non-KISS TCP client initiates the synthesize
        // handshake on connect + reconnect (Codeberg #64). The core-side
        // interface hash is registered in the per-interface config loop.
        tunnel_notify: Some(ctx.tunnel_notify_tx.clone()),
        socks_target: None,
        shutdown: None,
        outbound_socket_hook: ctx.outbound_socket_hook.clone(),
    });
    tracing::info!("TCP client interface for {} (reconnect enabled)", addr);
    Ok(Built::Handles(vec![handle]))
}

pub(super) fn build_server(
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    let listen_port = config
        .listen_port
        .ok_or_else(|| Error::Config("TCPServerInterface requires listen_port".to_string()))?;

    // A configured `device` binds to that kernel NIC's own address (Codeberg
    // #94, BackboneInterface.py:138-139); otherwise fall back to the
    // wildcard/`listen_ip` bind.
    let addr: SocketAddr = if let Some(device) = config.device.as_deref() {
        crate::interfaces::netdevice::resolve_if_bind_address(
            device,
            listen_port,
            config.prefer_ipv6.unwrap_or(false),
        )
        .map_err(|e| Error::Config(format!("TCPServerInterface device \"{}\": {}", device, e)))?
    } else {
        let listen_ip = config.listen_ip.as_deref().unwrap_or("0.0.0.0");
        format!("{}:{}", listen_ip, listen_port)
            .parse()
            .map_err(|e| Error::Config(format!("invalid listen address: {}", e)))?
    };

    let buffer_size = config.buffer_size.unwrap_or(TCP_DEFAULT_BUFFER_SIZE);
    let ifac = build_ifac_config(config);
    // Codeberg #104: resolve the listener's configured mode so each accepted
    // child inherits it (the listener itself does not register as an interface;
    // only spawned children do). An unknown mode string keeps the Full default,
    // matching Python.
    let mode = config
        .mode
        .as_deref()
        .and_then(leviculum_core::traits::InterfaceMode::from_config_str)
        .unwrap_or_default();
    spawn_tcp_server(
        addr,
        ctx.next_id.clone(),
        ctx.new_iface_tx.clone(),
        buffer_size,
        ctx.corrupt_every,
        ifac,
        mode,
    )?;
    Ok(Built::SelfManaged)
}
