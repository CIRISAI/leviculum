//! UDP interface builder.

use std::net::SocketAddr;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::udp::{spawn_udp_forward_only, spawn_udp_interface};
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

    // A single `port` key fills both the bind port and the forward port when
    // neither is set explicitly, matching rnsd (UDPInterface.py:68-72). On a
    // UDP block `config.port` (the shared string slot) is a port number, and
    // nothing else consumes it here, so a config brought over from a working
    // rnsd setup that names only `port` now starts instead of being rejected
    // (Codeberg #279). An explicit `listen_port` / `forward_port` wins.
    let shared_port: Option<u16> = config
        .port
        .as_deref()
        .and_then(|s| s.trim().parse::<u16>().ok());

    let listen_ip = config
        .listen_ip
        .as_deref()
        .or(device_broadcast.as_deref())
        .unwrap_or("0.0.0.0");
    let listen_port = config.listen_port.or(shared_port);
    let forward_ip = config.forward_ip.as_deref().or(device_broadcast.as_deref());
    let forward_port = config.forward_port.or(shared_port);

    // Bind and forward are two independent config blocks in the reference
    // (UDPInterface.py:89 and :110): a block carrying only bind parameters
    // listens, one carrying only forward parameters sends, and neither
    // demands the other's keys. Demanding both refused configs that rnsd
    // starts (Codeberg #279), so each side is assembled on its own and only
    // a block that would do nothing at all is a config error.
    //
    // `forward_ip` may hold several comma-separated entries (Rust-only
    // extension); each outgoing datagram goes to every one of them. Each
    // entry is an address or a hostname (Codeberg #148) — hostnames are
    // resolved by the interface at runtime, so a name that does not resolve
    // is an interface-level error here, not a config error, matching rnsd
    // (Python defers the lookup to sendto).
    let forward_targets = match forward_ip {
        Some(fwd) => match crate::interfaces::udp::parse_forward_addrs(fwd, forward_port) {
            Ok(targets) => targets,
            Err(crate::interfaces::udp::ForwardAddrError::MissingPort) => {
                // No forward port anywhere: the reference's forward block
                // simply does not fire. That is only tolerable if the bind
                // block does — otherwise the interface would be inert.
                if listen_port.is_none() {
                    return Err(Error::Config(
                        "UDPInterface requires forward_port or port".to_string(),
                    ));
                }
                tracing::warn!(
                    "UDP interface {} has forward_ip \"{}\" but no forward_port or port; \
                     it will receive only",
                    idx,
                    fwd
                );
                Vec::new()
            }
            Err(crate::interfaces::udp::ForwardAddrError::Invalid(msg)) => {
                return Err(Error::Config(format!(
                    "UDPInterface invalid forward address: {}",
                    msg
                )));
            }
        },
        None => Vec::new(),
    };

    let iface_name = format!("udp_{}", idx);
    let id = InterfaceId(idx);
    let forward_desc = forward_targets
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let handle = match listen_port {
        Some(port) => {
            let listen_addr: SocketAddr =
                format!("{}:{}", listen_ip, port).parse().map_err(|e| {
                    Error::Config(format!("UDPInterface invalid listen address: {}", e))
                })?;
            let handle = spawn_udp_interface(id, iface_name, listen_addr, forward_targets)?;
            if forward_desc.is_empty() {
                tracing::info!("UDP interface listening on {}, not forwarding", listen_addr);
            } else {
                tracing::info!(
                    "UDP interface listening on {}, forwarding to {}",
                    listen_addr,
                    forward_desc
                );
            }
            handle
        }
        None if !forward_targets.is_empty() => {
            let handle = spawn_udp_forward_only(id, iface_name, forward_targets)?;
            tracing::info!(
                "UDP interface forwarding to {}, not listening",
                forward_desc
            );
            handle
        }
        None => {
            return Err(Error::Config(
                "UDPInterface requires listen_port or port to receive, \
                 or forward_ip and forward_port to send"
                    .to_string(),
            ));
        }
    };

    Ok(Built::Handles(vec![handle]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InterfaceConfig;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    struct CtxOwner {
        next_id: Arc<AtomicUsize>,
        new_iface_tx: mpsc::Sender<crate::interfaces::InterfaceHandle>,
        reconnect_tx: mpsc::Sender<InterfaceId>,
        tunnel_notify_tx: mpsc::Sender<InterfaceId>,
        peer_event_tx: mpsc::Sender<(InterfaceId, crate::interfaces::PeerEvent)>,
        inventory: crate::interfaces::inventory::SharedInventory,
    }

    impl CtxOwner {
        fn new() -> Self {
            let (new_iface_tx, _) = mpsc::channel(4);
            let (reconnect_tx, _) = mpsc::channel(4);
            let (tunnel_notify_tx, _) = mpsc::channel(4);
            let (peer_event_tx, _) = mpsc::channel(4);
            Self {
                next_id: Arc::new(AtomicUsize::new(100)),
                new_iface_tx,
                reconnect_tx,
                tunnel_notify_tx,
                peer_event_tx,
                inventory: crate::interfaces::inventory::InterfaceInventory::shared(),
            }
        }

        fn ctx(&self) -> InterfaceBuildCtx<'_> {
            InterfaceBuildCtx {
                next_id: &self.next_id,
                new_iface_tx: &self.new_iface_tx,
                reconnect_tx: &self.reconnect_tx,
                tunnel_notify_tx: &self.tunnel_notify_tx,
                peer_event_tx: &self.peer_event_tx,
                corrupt_every: None,
                storage_path: None,
                outbound_socket_hook: None,
                inventory: self.inventory.clone(),
                transport_enabled: false,
                identity_hash: [0x5A; 16],
            }
        }
    }

    /// A single `port` key fills both the bind and the forward port, so a UDP
    /// block that names only `port` (plus a forward address) builds instead of
    /// being rejected for a missing `listen_port` (Codeberg #279, rnsd
    /// UDPInterface.py:68-72). Port 0 keeps the bind ephemeral so the test
    /// never races a fixed port.
    #[tokio::test]
    async fn single_port_key_fills_bind_and_forward_ports() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "UDPInterface".to_string(),
            port: Some("0".to_string()),
            forward_ip: Some("127.0.0.1".to_string()),
            ..Default::default()
        };
        let built =
            build(0, &config, &owner.ctx()).expect("a UDP block with only `port` must build");
        let Built::Handles(handles) = built else {
            panic!("UDP builds one handle");
        };
        assert_eq!(handles.len(), 1);
    }

    /// Forward parameters alone build a send-only interface: the reference's
    /// bind and forward blocks are independent (UDPInterface.py:89 and :110),
    /// so a block with no bind port is not an error, it just does not listen
    /// (Codeberg #279).
    #[tokio::test]
    async fn forward_parameters_alone_build_a_send_only_interface() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "UDPInterface".to_string(),
            forward_ip: Some("127.0.0.1".to_string()),
            forward_port: Some(4242),
            ..Default::default()
        };
        let built = build(0, &config, &owner.ctx())
            .expect("a UDP block with only forward parameters must build");
        let Built::Handles(handles) = built else {
            panic!("UDP builds one handle");
        };
        assert_eq!(handles.len(), 1);
    }

    /// Bind parameters alone build a receive-only interface: no forward
    /// address is demanded (UDPInterface.py:89, Codeberg #279). Port 0 keeps
    /// the bind ephemeral so the test never races a fixed port.
    #[tokio::test]
    async fn bind_parameters_alone_build_a_receive_only_interface() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "UDPInterface".to_string(),
            listen_ip: Some("127.0.0.1".to_string()),
            listen_port: Some(0),
            ..Default::default()
        };
        let built = build(0, &config, &owner.ctx())
            .expect("a UDP block with only bind parameters must build");
        let Built::Handles(handles) = built else {
            panic!("UDP builds one handle");
        };
        assert_eq!(handles.len(), 1);
    }

    /// A `device` key fills both the bind and the forward address from the
    /// NIC's broadcast address (UDPInterface.py:82-86), and with a bind port
    /// present the missing `forward_port` no longer refuses the interface
    /// (Codeberg #279).
    ///
    /// The broadcast address has to come from a real NIC, so the device is
    /// taken from the same enumeration the production resolver uses. A host
    /// with no broadcast-capable IPv4 interface (an isolated netns with only
    /// loopback) cannot exercise this form at all; the test says so rather
    /// than failing on an environment it does not test.
    // Windows refuses to bind a broadcast address (WSAEADDRNOTAVAIL, 10049).
    // The device form binds one, as UDPInterface.py does, so it shares that
    // limit there (CIRIS fork: the Windows lane).
    #[cfg_attr(
        windows,
        ignore = "Windows cannot bind a broadcast address; the device form mirrors UDPInterface.py"
    )]
    #[tokio::test]
    async fn a_device_fills_both_addresses_and_needs_no_forward_port() {
        let Some(device) = if_addrs::get_if_addrs()
            .expect("enumerating interfaces")
            .into_iter()
            .find(|i| matches!(&i.addr, if_addrs::IfAddr::V4(a) if a.broadcast.is_some()))
            .map(|i| i.name)
        else {
            eprintln!("no broadcast-capable IPv4 interface on this host; device form not covered");
            return;
        };
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "UDPInterface".to_string(),
            device: Some(device.clone()),
            listen_port: Some(0),
            ..Default::default()
        };
        let built = build(0, &config, &owner.ctx())
            .unwrap_or_else(|e| panic!("a UDP block with device \"{device}\" must build: {e}"));
        let Built::Handles(handles) = built else {
            panic!("UDP builds one handle");
        };
        assert_eq!(handles.len(), 1);
    }

    /// A block that would neither receive nor send is the one genuinely bad
    /// config, and the error names both ways out of it.
    #[test]
    fn neither_bind_nor_forward_is_a_named_error() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "UDPInterface".to_string(),
            ..Default::default()
        };
        let err = build(0, &config, &owner.ctx())
            .err()
            .expect("a UDP block with no addresses at all must not build");
        assert!(err.to_string().contains("listen_port or port"), "{err}");
        assert!(err.to_string().contains("forward_ip"), "{err}");
    }

    /// A forward address with no port anywhere and no bind port is inert:
    /// the reference's forward block needs `forwardport`, so there is
    /// nothing left for the interface to do.
    #[test]
    fn forward_address_without_any_port_is_a_named_error() {
        let owner = CtxOwner::new();
        let config = InterfaceConfig {
            interface_type: "UDPInterface".to_string(),
            forward_ip: Some("127.0.0.1".to_string()),
            ..Default::default()
        };
        let err = build(0, &config, &owner.ctx())
            .err()
            .expect("a forward address with no port must not build");
        assert!(err.to_string().contains("forward_port or port"), "{err}");
    }

    /// End to end from the config file: an rnsd-style config file carrying
    /// all three forms — a single `port`, bind parameters only, forward
    /// parameters only — parses and every one of its interfaces starts
    /// (Codeberg #279). The forms reach the builder only through this
    /// parser, so the constructed-`InterfaceConfig` tests above do not cover
    /// the keys actually surviving the file.
    #[tokio::test]
    async fn an_rnsd_style_config_file_with_all_three_forms_starts() {
        let config = crate::ini_config::parse_ini(
            r#"
[interfaces]
  [[UDP shared port]]
    type = UDPInterface
    enabled = yes
    port = 0
    forward_ip = 127.0.0.1
  [[UDP bind only]]
    type = UDPInterface
    enabled = yes
    listen_ip = 127.0.0.1
    listen_port = 0
  [[UDP forward only]]
    type = UDPInterface
    enabled = yes
    forward_ip = 127.0.0.1
    forward_port = 4242
"#,
        )
        .expect("config must parse");

        let owner = CtxOwner::new();
        for (idx, (name, iface)) in config.interfaces.iter().enumerate() {
            let built = build(idx, iface, &owner.ctx())
                .unwrap_or_else(|e| panic!("interface \"{name}\" must start: {e}"));
            let Built::Handles(handles) = built else {
                panic!("UDP builds one handle");
            };
            assert_eq!(handles.len(), 1, "interface \"{name}\"");
        }
        assert_eq!(config.interfaces.len(), 3);
    }
}
