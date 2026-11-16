//! TCP client and server interface builders.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::interfaces::inventory::{self, InterfaceIdentity};
use crate::interfaces::tcp::{
    spawn_tcp_client_with_reconnect, spawn_tcp_server, TcpClientConfig, TcpLiveness,
    TcpServerConfig, TcpTarget, DEFAULT_RECONNECT_MAX_INTERVAL, DEFAULT_TCP_CONNECT_TIMEOUT,
    TCP_DEFAULT_BUFFER_SIZE,
};
use crate::interfaces::tcp_admission::{
    AdmissionConfig, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_CONNECTIONS_PER_SOURCE,
    DEFAULT_PRESSURE_FRACTION, DEFAULT_SILENT_TIMEOUT_PRESSURE_SECS, DEFAULT_SILENT_TIMEOUT_SECS,
};
use crate::sync_ext::MutexRecover;
use leviculum_core::transport::InterfaceId;

use super::super::{build_announce_rate_config, build_ifac_config};
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

    // Not resolved here: the interface looks the name up on every connect
    // attempt, so a name that does not resolve yet leaves the interface down
    // and retrying instead of ending the start (Codeberg #461; the reference
    // resolves inside `TCPInterface.connect` and keeps going). Only what the
    // config itself got wrong, above, is an error.
    //
    // An automatic server slot (#471 step 3) starts on the entry the slot
    // holds now, which after a fail-over is no longer the section's own
    // target: a `bootstrap_only` slot re-attached later comes back on the
    // entry that last worked.
    let target = match config.server_slot.as_ref().and_then(|s| s.current()) {
        Some(entry) => TcpTarget::new(entry.host, entry.port),
        None => TcpTarget::new(target_host.clone(), target_port),
    };

    let iface_name = format!("tcp_client_{}", idx);
    let id = InterfaceId(idx);
    let buffer_size = config.buffer_size.unwrap_or(TCP_DEFAULT_BUFFER_SIZE);
    let reconnect_interval = Duration::from_secs(config.reconnect_interval_secs.unwrap_or(5));
    let liveness = liveness_config(config)?;

    // TCP interfaces don't register a bitrate cap (bitrate=0 means unlimited).
    // Future LoRa/serial interfaces should call register_interface_bitrate(id,
    // bitrate) after registration to enable per-interface announce caps.
    let handle = spawn_tcp_client_with_reconnect(TcpClientConfig {
        id,
        name: iface_name,
        target: target.clone(),
        resolver: ctx.host_resolver.clone(),
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
        liveness,
        failover: config.server_slot.clone(),
    });
    // The reference names this interface `TCPInterface[<name>/<host>:<port>]`
    // in every answer (D8 of the 2026-10-11 shared-instance audit); the
    // driver's `tcp_client_<idx>` stays the internal id, and the tunnel hash
    // keeps deriving from it (`tcp_client_tunnel_hash`). A programmatic
    // client has no section name and keeps the internal one.
    if !config.name.is_empty() {
        ctx.inventory.lock_recover().add_spawned(
            idx,
            InterfaceIdentity {
                name: inventory::tcp_client_name(&config.name, target_host, target_port),
                short_name: config.name.clone(),
                type_name: "TCPClientInterface",
                parent: None,
            },
        );
    }
    tracing::info!("TCP client interface for {} (reconnect enabled)", target);
    Ok(Built::Handles(vec![handle]))
}

pub(super) fn build_server(
    idx: usize,
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
        super::bind_address(listen_ip, listen_port, config.prefer_ipv6.unwrap_or(false))
            .map_err(|e| Error::Config(format!("invalid listen address \"{listen_ip}\": {e}")))?
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
    // The listener never becomes a routable interface, so `idx` — the config
    // index, which the id allocator skips for exactly this reason — is free to
    // identify it in the reporting inventory (Codeberg #177). Ids there and in
    // transport come from the same space, so a listener id can never collide
    // with a spawned child's.
    // A listener the builder was handed pre-bound (`add_tcp_server_listener`)
    // is taken out of its slot here, once; `addr` then only names it. The
    // slot is a Mutex purely so `InterfaceConfig` stays `Clone`.
    let prebound = config.prebound_tcp_listener.as_ref().and_then(|slot| {
        slot.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    });

    spawn_tcp_server(TcpServerConfig {
        bind_addr: addr,
        prebound,
        section: config.name.clone(),
        next_id: ctx.next_id.clone(),
        new_interface_tx: ctx.new_iface_tx.clone(),
        buffer_size,
        corrupt_every: ctx.corrupt_every,
        ifac,
        mode,
        // leviculum#51: declared transit policy; every accepted connection
        // inherits it (the listener itself never routes).
        transit: config.transit.unwrap_or(true),
        // Codeberg #189: the listener resolves its ingress control once and
        // every accepted connection inherits it. The listener itself never
        // registers as a routable interface, so the startup config loop (which
        // sets the flag by config index) skips it — this is the only place the
        // operator's `ingress_control` on a TCP server can take effect.
        ingress_control: config.resolve_ingress_control(),
        listener_id: idx,
        inventory: Arc::clone(&ctx.inventory),
        announce_rate: leviculum_core::transport::resolve_announce_rate(
            build_announce_rate_config(config).as_ref(),
            ctx.transport_enabled,
            &ctx.default_announce_rate,
        ),
        admission: admission_config(config)?,
        // Every accepted connection takes the listener's liveness, as it
        // takes its mode: the section is the only place a config file has
        // for an accepted connection (1176).
        liveness: liveness_config(config)?,
    })?;
    Ok(Built::SelfManaged)
}

/// The TCP server's admission settings (1190), defaults for every key the
/// section leaves out.
pub(super) fn admission_config(config: &InterfaceConfig) -> Result<AdmissionConfig, Error> {
    let pressure_fraction = config
        .pressure_fraction
        .unwrap_or(DEFAULT_PRESSURE_FRACTION);
    // A fraction of zero or less puts every server under pressure from the
    // first connection; above one never. Both are typos, not policies.
    if !(pressure_fraction > 0.0 && pressure_fraction <= 1.0) {
        return Err(Error::Config(format!(
            "interface '{}': pressure_fraction must be above 0 and at most 1, got {}",
            config.name, pressure_fraction
        )));
    }
    Ok(AdmissionConfig {
        max_connections_per_source: config
            .max_connections_per_source
            .unwrap_or(DEFAULT_MAX_CONNECTIONS_PER_SOURCE),
        max_connections: config.max_connections.unwrap_or(DEFAULT_MAX_CONNECTIONS),
        silent_timeout: Duration::from_secs(
            config
                .silent_timeout_secs
                .unwrap_or(DEFAULT_SILENT_TIMEOUT_SECS),
        ),
        silent_timeout_pressure: Duration::from_secs(
            config
                .silent_timeout_pressure_secs
                .unwrap_or(DEFAULT_SILENT_TIMEOUT_PRESSURE_SECS),
        ),
        pressure_fraction,
    })
}

/// A TCP client's or server's keepalive and user timeout (1176), today's
/// defaults for every key the section leaves out. A value the kernel would
/// refuse is an error here: at the socket it would fail silently.
pub(super) fn liveness_config(config: &InterfaceConfig) -> Result<TcpLiveness, Error> {
    TcpLiveness::from_keys(
        config.tcp_keepalive_idle_secs,
        config.tcp_keepalive_interval_secs,
        config.tcp_keepalive_probes,
        config.tcp_user_timeout_secs,
    )
    .map_err(|detail| Error::Config(format!("interface '{}': {detail}", config.name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(lines: &str) -> InterfaceConfig {
        let config = crate::ini_config::parse_ini(&format!(
            "[interfaces]\n  [[Phones]]\n    type = TCPServerInterface\n    \
             listen_port = 4243\n{lines}"
        ))
        .expect("config parses");
        config.interfaces.get("Phones").expect("section").clone()
    }

    /// The four liveness keys (1176) survive the round trip from the config
    /// file through `InterfaceConfig` and its serde form to the socket
    /// settings, and a section without them keeps today's values.
    #[test]
    fn liveness_keys_round_trip_from_the_config_file() {
        let section = parsed(
            "    tcp_keepalive_idle_secs = 600\n    tcp_keepalive_interval_secs = 30\n    \
             tcp_keepalive_probes = 4\n    tcp_user_timeout_secs = 90\n",
        );
        assert_eq!(section.tcp_keepalive_idle_secs, Some(600));
        assert_eq!(section.tcp_keepalive_interval_secs, Some(30));
        assert_eq!(section.tcp_keepalive_probes, Some(4));
        assert_eq!(section.tcp_user_timeout_secs, Some(90));
        let json = serde_json::to_string(&section).expect("serialises");
        let back: InterfaceConfig = serde_json::from_str(&json).expect("deserialises");
        let liveness = liveness_config(&back).expect("in range");
        assert_eq!(
            liveness,
            TcpLiveness {
                idle: Duration::from_secs(600),
                interval: Duration::from_secs(30),
                probes: 4,
                user_timeout: Some(Duration::from_secs(90)),
            }
        );
        assert_eq!(
            liveness_config(&parsed("")).expect("defaults"),
            TcpLiveness::default()
        );
    }

    /// 1346: a silent peer is noticed after the probe idle plus one probe
    /// interval when the user timeout is shorter than the idle, and the
    /// defaults keep today's 26 s.
    #[test]
    fn a_silent_peer_is_dropped_after_the_idle_and_one_probe() {
        assert_eq!(
            parsed("").tcp_silent_peer_bound(),
            Ok(Duration::from_secs(26))
        );
        assert_eq!(
            parsed("    tcp_keepalive_idle_secs = 240\n").tcp_silent_peer_bound(),
            Ok(Duration::from_secs(242))
        );
        assert_eq!(
            parsed("    tcp_keepalive_idle_secs = 240\n    tcp_user_timeout_secs = 0\n")
                .tcp_silent_peer_bound(),
            Ok(Duration::from_secs(240 + 2 * 12)),
            "no user timeout: the probes count"
        );
        assert!(parsed("    tcp_keepalive_idle_secs = 0\n")
            .tcp_silent_peer_bound()
            .is_err());
    }

    /// A value the kernel would refuse stops the start with the section's
    /// name, instead of failing silently at the socket.
    #[test]
    fn an_out_of_range_liveness_key_is_a_config_error() {
        let error =
            liveness_config(&parsed("    tcp_keepalive_idle_secs = 0\n")).expect_err("zero idle");
        let text = error.to_string();
        assert!(text.contains("Phones"), "{text}");
        assert!(text.contains("tcp_keepalive_idle_secs"), "{text}");
    }
}
