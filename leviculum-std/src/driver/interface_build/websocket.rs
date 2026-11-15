//! +ciris (#109): WebSocket server and client builders, the config-file side
//! of [`crate::interfaces::websocket`], and the builder methods that add them
//! programmatically.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::inventory::InterfaceIdentity;
use crate::interfaces::tcp::{
    DEFAULT_RECONNECT_MAX_INTERVAL, DEFAULT_TCP_CONNECT_TIMEOUT, TCP_DEFAULT_BUFFER_SIZE,
};
use crate::interfaces::websocket::{
    spawn_ws_client_with_reconnect, spawn_ws_server, WsClientConfig, WsFraming, WsServerConfig,
    WsTarget,
};
use crate::sync_ext::MutexRecover;
use leviculum_core::transport::InterfaceId;

use super::super::{build_announce_rate_config, build_ifac_config, ReticulumNode};
use super::tcp::{admission_config, liveness_config};
use super::{Built, InterfaceBuildCtx};

/// The config type of a WebSocket listener.
pub(crate) const WS_SERVER_TYPE: &str = "WebSocketServerInterface";
/// The config type of a WebSocket client.
pub(crate) const WS_CLIENT_TYPE: &str = "WebSocketClientInterface";

pub(super) fn build_client(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    let target = match config.websocket_url.as_deref() {
        Some(url) => WsTarget::parse_url(url)
            .map_err(|e| Error::Config(format!("interface '{}': url {e}", config.name)))?,
        None => {
            let target_host = config.target_host.as_ref().ok_or_else(|| {
                Error::Config(format!("{WS_CLIENT_TYPE} requires target_host or url"))
            })?;
            let tls = config.websocket_tls.unwrap_or(false);
            let target_port = config.target_port.unwrap_or(if tls { 443 } else { 80 });
            let path = config.websocket_path.as_deref().unwrap_or("/");
            WsTarget::new(target_host.clone(), target_port, path, tls)
        }
    };
    let framing = match config.websocket_framing.as_deref() {
        None => WsFraming::Raw,
        Some(value) => WsFraming::from_config(value).ok_or_else(|| {
            Error::Config(format!(
                "interface '{}': framing = '{value}' is neither raw nor hdlc",
                config.name
            ))
        })?,
    };

    let handle = spawn_ws_client_with_reconnect(WsClientConfig {
        id: InterfaceId(idx),
        name: format!("ws_client_{idx}"),
        target: target.clone(),
        resolver: ctx.host_resolver.clone(),
        buffer_size: config.buffer_size.unwrap_or(TCP_DEFAULT_BUFFER_SIZE),
        reconnect_interval: Duration::from_secs(config.reconnect_interval_secs.unwrap_or(5)),
        max_reconnect_tries: config.max_reconnect_tries,
        reconnect_max_interval: DEFAULT_RECONNECT_MAX_INTERVAL,
        connect_timeout: DEFAULT_TCP_CONNECT_TIMEOUT,
        reconnect_notify: Some(ctx.reconnect_tx.clone()),
        outbound_socket_hook: ctx.outbound_socket_hook.clone(),
        liveness: liveness_config(config)?,
        framing,
    });
    if !config.name.is_empty() {
        ctx.inventory.lock_recover().add_spawned(
            idx,
            InterfaceIdentity {
                name: format!("WebSocketInterface[{}/{}]", config.name, target),
                short_name: config.name.clone(),
                type_name: WS_CLIENT_TYPE,
                parent: None,
            },
        );
    }
    tracing::info!(
        "WebSocket client interface for {} (reconnect enabled)",
        target
    );
    Ok(Built::Handles(vec![handle]))
}

pub(super) fn build_server(
    idx: usize,
    config: &InterfaceConfig,
    ctx: &InterfaceBuildCtx<'_>,
) -> Result<Built, Error> {
    let listen_port = config
        .listen_port
        .ok_or_else(|| Error::Config(format!("{WS_SERVER_TYPE} requires listen_port")))?;
    let addr: SocketAddr = if let Some(device) = config.device.as_deref() {
        crate::interfaces::netdevice::resolve_if_bind_address(
            device,
            listen_port,
            config.prefer_ipv6.unwrap_or(false),
        )
        .map_err(|e| Error::Config(format!("{WS_SERVER_TYPE} device \"{device}\": {e}")))?
    } else {
        let listen_ip = config.listen_ip.as_deref().unwrap_or("0.0.0.0");
        super::bind_address(listen_ip, listen_port, config.prefer_ipv6.unwrap_or(false))
            .map_err(|e| Error::Config(format!("invalid listen address \"{listen_ip}\": {e}")))?
    };
    let mode = config
        .mode
        .as_deref()
        .and_then(leviculum_core::traits::InterfaceMode::from_config_str)
        .unwrap_or_default();
    let prebound = config
        .prebound_tcp_listener
        .as_ref()
        .and_then(|slot| slot.lock_recover().take());

    spawn_ws_server(WsServerConfig {
        bind_addr: addr,
        prebound,
        section: config.name.clone(),
        next_id: ctx.next_id.clone(),
        new_interface_tx: ctx.new_iface_tx.clone(),
        buffer_size: config.buffer_size.unwrap_or(TCP_DEFAULT_BUFFER_SIZE),
        ifac: build_ifac_config(config),
        mode,
        transit: config.transit.unwrap_or(true),
        ingress_control: config.resolve_ingress_control(),
        listener_id: idx,
        inventory: Arc::clone(&ctx.inventory),
        announce_rate: leviculum_core::transport::resolve_announce_rate(
            build_announce_rate_config(config).as_ref(),
            ctx.transport_enabled,
            &ctx.default_announce_rate,
        ),
        admission: admission_config(config)?,
        liveness: liveness_config(config)?,
    })?;
    Ok(Built::SelfManaged)
}

impl crate::driver::ReticulumNodeBuilder {
    /// Add a WebSocket server (`WebSocketServerInterface`, #109): accepts
    /// `ws://` connections on `addr`, one Reticulum packet per binary
    /// message, under the TCP server's admission control. TLS (`wss://`) is
    /// a reverse proxy's job in front of it.
    pub fn add_websocket_server(self, addr: SocketAddr) -> Self {
        self.add_interface_config(InterfaceConfig {
            name: "WebSocket Server".to_string(),
            interface_type: WS_SERVER_TYPE.to_string(),
            listen_ip: Some(addr.ip().to_string()),
            listen_port: Some(addr.port()),
            ..Default::default()
        })
    }

    /// [`add_websocket_server`](Self::add_websocket_server) on a listener
    /// the caller already bound, so the port is held from the caller's bind
    /// on (as [`add_tcp_server_listener`](Self::add_tcp_server_listener)).
    pub fn add_websocket_server_listener(self, listener: std::net::TcpListener) -> Self {
        let addr = listener.local_addr().ok();
        self.add_interface_config(InterfaceConfig {
            name: "WebSocket Server".to_string(),
            interface_type: WS_SERVER_TYPE.to_string(),
            listen_ip: addr.map(|a| a.ip().to_string()),
            listen_port: addr.map(|a| a.port()),
            prebound_tcp_listener: Some(Arc::new(std::sync::Mutex::new(Some(listener)))),
            ..Default::default()
        })
    }

    /// Add a WebSocket client (`WebSocketClientInterface`, #109) that dials
    /// `ws://host:port/path`, or `wss://` when `tls` is set, and redials
    /// with the TCP client's backoff when the connection ends.
    pub fn add_websocket_client(
        self,
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
        tls: bool,
    ) -> Self {
        self.add_interface_config(InterfaceConfig {
            name: "WebSocket Client".to_string(),
            interface_type: WS_CLIENT_TYPE.to_string(),
            target_host: Some(host.into()),
            target_port: Some(port),
            websocket_path: Some(path.into()),
            websocket_tls: Some(tls),
            ..Default::default()
        })
    }
}

impl crate::driver::ReticulumNodeBuilder {
    /// Add a WebSocket client from one URL, `ws://host[:port][/path]` or
    /// `wss://...` (port 80 or 443 by default, path `/`), with the default
    /// `rns.raw` framing (#109). A future browser driver offers the same
    /// call. A URL that does not parse is an error when the node builds.
    pub fn add_websocket_interface(self, url: impl Into<String>) -> Self {
        self.add_interface_config(InterfaceConfig {
            name: "WebSocket Client".to_string(),
            interface_type: WS_CLIENT_TYPE.to_string(),
            websocket_url: Some(url.into()),
            ..Default::default()
        })
    }
}

impl ReticulumNode {
    /// The addresses this node's WebSocket servers listen on, port 0
    /// resolved to the port the kernel gave (#109). The WebSocket twin of
    /// [`tcp_listen_addrs`](Self::tcp_listen_addrs), which leaves them out.
    pub fn websocket_listen_addrs(&self) -> Vec<SocketAddr> {
        let inv = self.inventory.lock_recover();
        inv.listeners()
            .filter(|(_, row)| row.identity.type_name == WS_SERVER_TYPE)
            .filter_map(|(_, row)| row.bound_addr)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(text: &str, name: &str) -> InterfaceConfig {
        let config = crate::ini_config::parse_ini(text).expect("config parses");
        config.interfaces.get(name).expect("section").clone()
    }

    /// Both sections parse from a config file with every WebSocket key, and
    /// the client's role default keeps ingress control off, as a TCP
    /// client's does, while the listener's is on.
    #[test]
    fn websocket_sections_parse_from_the_config_file() {
        let text = "[interfaces]\n  [[Browsers]]\n    type = WebSocketServerInterface\n    \
                    listen_ip = 127.0.0.1\n    listen_port = 8765\n    \
                    max_connections_per_source = 4\n  [[Canonical]]\n    \
                    type = WebSocketClientInterface\n    target_host = mesh.example.org\n    \
                    target_port = 443\n    path = /rns\n    tls = yes\n    framing = hdlc\n  [[ByUrl]]\n    \
                    type = WebSocketClientInterface\n    url = wss://[2001:db8::1]/rns\n";
        let server = section(text, "Browsers");
        assert_eq!(server.interface_type, WS_SERVER_TYPE);
        assert_eq!(server.listen_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(server.listen_port, Some(8765));
        assert_eq!(
            admission_config(&server)
                .expect("admission")
                .max_connections_per_source,
            4
        );
        assert!(server.resolve_ingress_control());

        let client = section(text, "Canonical");
        assert_eq!(client.interface_type, WS_CLIENT_TYPE);
        assert_eq!(client.target_host.as_deref(), Some("mesh.example.org"));
        assert_eq!(client.target_port, Some(443));
        assert_eq!(client.websocket_path.as_deref(), Some("/rns"));
        assert_eq!(client.websocket_tls, Some(true));
        assert!(!client.resolve_ingress_control());
        assert_eq!(client.websocket_framing.as_deref(), Some("hdlc"));
        let by_url = section(text, "ByUrl");
        assert_eq!(
            WsTarget::parse_url(by_url.websocket_url.as_deref().expect("url")),
            Ok(WsTarget::new("2001:db8::1", 443, "/rns", true))
        );

        // The serde form (what the builder and the TOML path read) keeps
        // the WebSocket keys too.
        let json = serde_json::to_string(&client).expect("serialises");
        let back: InterfaceConfig = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back.websocket_path.as_deref(), Some("/rns"));
        assert_eq!(back.websocket_tls, Some(true));
    }

    /// A malformed `tls` value is an error naming the key, not a silent
    /// plain-text dial.
    #[test]
    fn a_bad_tls_value_is_refused() {
        let text = "[interfaces]\n  [[C]]\n    type = WebSocketClientInterface\n    \
                    target_host = h\n    tls = perhaps\n";
        let err = crate::ini_config::parse_ini(text).expect_err("bad bool");
        assert!(err.to_string().contains("tls"), "{err}");
    }
}
