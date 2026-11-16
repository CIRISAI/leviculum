//! +ciris (leviculum#101): the node side of port mapping: the builder
//! methods, which ports a run maps, starting and stopping the task, and the
//! accessors a host reads. The protocols and the schedule live in
//! [`crate::port_mapping`]; this file is all the driver adds, so no cited
//! line of upstream's driver moves.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_core::node::NodeEvent;
use tokio::sync::watch;

use super::{ReticulumNode, ReticulumNodeBuilder};
use crate::config::InterfaceConfig;
use crate::error::Error;
use crate::port_mapping::engine::{PortKey, Shared};
use crate::port_mapping::{
    MappingProtocol, MappingState, PortMappingConfig, PortMappingStatus, ReachabilityReport,
};
use crate::sync_ext::MutexRecover;

/// How long a stop waits for the task to delete its mappings. A request in
/// flight finishes first (bounded by the engine's per-request timeout), so
/// a mapping is never made behind the stop's back.
const STOP_BOUND: Duration = Duration::from_secs(12);

/// The port-mapping part of a node: its settings, and what the task shares.
pub(crate) struct NodePortMapping {
    config: PortMappingConfig,
    shared: Arc<Mutex<Shared>>,
}

impl Default for NodePortMapping {
    /// Off: only the builder (`configure`) turns it on, so a node made any
    /// other way maps nothing.
    fn default() -> Self {
        NodePortMapping {
            config: PortMappingConfig {
                enabled: false,
                ..Default::default()
            },
            shared: Arc::default(),
        }
    }
}

impl NodePortMapping {
    /// The shared state: a leaf lock, never the core's and never held
    /// across an await.
    fn lock(&self) -> impl std::ops::DerefMut<Target = Shared> + '_ {
        self.shared.lock_recover()
    }
}

/// What one run's task owns, kept in the run state.
pub(crate) struct RunHandle {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ReticulumNodeBuilder {
    /// +ciris (leviculum#101): port-mapping settings, overriding the
    /// config's `port_mapping` and `port_mapping_gateway`. On by default:
    /// the node asks the router (PCP, then NAT-PMP, then UPnP-IGD) to open
    /// its listening ports to the internet, in the background, and logs
    /// each mapping at info level (without the external address, which is
    /// at debug).
    pub fn port_mapping(mut self, config: PortMappingConfig) -> Self {
        self.port_map_cfg = Some(config);
        self
    }

    /// +ciris (leviculum#101): the switch alone, every other setting at
    /// its default. `enable_port_mapping(false)` turns port mapping off,
    /// whatever the config says.
    pub fn enable_port_mapping(self, enabled: bool) -> Self {
        self.port_mapping(PortMappingConfig {
            enabled,
            ..Default::default()
        })
    }
}

/// Settle the node's settings at build time: the builder's over the
/// config's, refused when this build or the values cannot honour them.
pub(super) fn configure(
    node: &mut ReticulumNode,
    explicit: Option<PortMappingConfig>,
    reticulum: &crate::config::ReticulumConfig,
) -> Result<(), Error> {
    let config = match explicit {
        Some(c) => c,
        None => PortMappingConfig::from_config(reticulum)?,
    };
    config.validate()?;
    node.port_mapping.config = config;
    Ok(())
}

/// The listener ports of `interfaces`: the TCP and WebSocket servers not
/// bound to loopback, with an IPv6 pinhole too for one bound to an IPv6
/// address.
fn listener_keys(interfaces: &[InterfaceConfig], ipv6_pinholes: bool) -> Vec<PortKey> {
    let mut keys = Vec::new();
    for config in interfaces.iter().filter(|c| c.enabled) {
        let is_listener = config.interface_type == "TCPServerInterface"
            || config.interface_type == "WebSocketServerInterface";
        let Some(port) = config.listen_port.filter(|p| *p != 0) else {
            continue;
        };
        if !is_listener {
            continue;
        }
        let ip: Option<IpAddr> = config
            .listen_ip
            .as_deref()
            .and_then(|s| s.trim().parse().ok());
        let (v4, v6) = match ip {
            Some(ip) if ip.is_loopback() => (false, false),
            Some(IpAddr::V6(v6)) if v6.is_unspecified() => (true, true),
            Some(IpAddr::V6(_)) => (false, true),
            // The default 0.0.0.0, an IPv4 address, or a name.
            _ => (
                true,
                config.prefer_ipv6 == Some(true) && config.device.is_some(),
            ),
        };
        if v4 {
            keys.push(PortKey {
                protocol: MappingProtocol::Tcp,
                internal_port: port,
                v6: false,
            });
        }
        if v6 && ipv6_pinholes {
            keys.push(PortKey {
                protocol: MappingProtocol::Tcp,
                internal_port: port,
                v6: true,
            });
        }
    }
    keys
}

/// Every port a run maps, deduplicated, in config order.
fn run_keys(node: &ReticulumNode) -> Vec<PortKey> {
    let config = &node.port_mapping.config;
    let mut keys = Vec::new();
    if config.map_listeners {
        keys.extend(listener_keys(&node.interfaces, config.ipv6_pinholes));
        if let Some(port) = node.direct_link.facilitator_port.filter(|p| *p != 0) {
            keys.push(PortKey {
                protocol: MappingProtocol::Udp,
                internal_port: port,
                v6: false,
            });
        }
    }
    keys.extend(config.extra_ports.iter().map(|(protocol, port)| PortKey {
        protocol: *protocol,
        internal_port: *port,
        v6: false,
    }));
    let mut seen = std::collections::HashSet::new();
    keys.retain(|k| seen.insert(*k));
    keys
}

/// Turn a status change into the host's event, on the control plane.
fn event_sink(node: &ReticulumNode) -> crate::port_mapping::engine::ChangeSink {
    let tx = node.control_tx.clone();
    let dropped = Arc::clone(&node.control_dropped);
    Box::new(move |s: &PortMappingStatus| {
        let Some(tx) = &tx else { return };
        let external = if s.state == MappingState::Mapped {
            s.external
        } else {
            None
        };
        let event = NodeEvent::PortMappingChanged {
            internal_port: s.internal_port,
            udp: s.protocol == MappingProtocol::Udp,
            external,
            method: s.method.map(|m| m.as_str()).unwrap_or(""),
        };
        if tx.try_send(event).is_err() {
            // Counted like any control event the channel could not take, so
            // the receiver's overflow marker reports it.
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    })
}

/// Start this run's port-mapping task, on the node's runtime (the caller
/// has entered it). `None` when port mapping is off or there is nothing to
/// map.
pub(super) fn start(node: &ReticulumNode) -> Option<RunHandle> {
    let config = &node.port_mapping.config;
    if !config.enabled {
        return None;
    }
    if node.connect_instance_name.is_some() {
        tracing::info!(
            "port mapping: this node is a shared-instance client; the shared instance maps \
             its own listeners"
        );
        return None;
    }
    let keys = run_keys(node);
    {
        let mut shared = node.port_mapping.lock();
        shared.statuses.clear();
    }
    if keys.is_empty() {
        tracing::debug!("port mapping: on, but this node runs no listener to map");
        return None;
    }
    tracing::info!(
        "port mapping: on; will ask the router to open {} to the internet",
        keys.iter()
            .map(|k| format!(
                "{} {}{}",
                k.protocol.as_str(),
                k.internal_port,
                if k.v6 { " (IPv6)" } else { "" }
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    spawn_engine(node, keys)
}

#[cfg(feature = "port-mapping")]
fn spawn_engine(node: &ReticulumNode, keys: Vec<PortKey>) -> Option<RunHandle> {
    let config = node.port_mapping.config.clone();
    let want_v6 = keys.iter().any(|k| k.v6);
    let lease_secs = u32::try_from(config.lease.as_secs()).unwrap_or(u32::MAX);
    let discover = crate::port_mapping::clients::SystemDiscover::new(config, want_v6);
    let engine = crate::port_mapping::engine::Engine::new(
        keys,
        lease_secs,
        Box::new(discover),
        Arc::clone(&node.port_mapping.shared),
        event_sink(node),
    );
    let (shutdown, rx) = watch::channel(false);
    let task = tokio::spawn(engine.run(rx));
    Some(RunHandle { shutdown, task })
}

#[cfg(not(feature = "port-mapping"))]
fn spawn_engine(_node: &ReticulumNode, _keys: Vec<PortKey>) -> Option<RunHandle> {
    // `configure` refuses port_mapping = yes without the feature.
    let _ = event_sink;
    None
}

/// Delete this run's mappings and end its task, on the node's runtime,
/// before that runtime is shut down.
pub(super) async fn stop(handle: Option<RunHandle>, runtime: Option<&tokio::runtime::Runtime>) {
    let Some(RunHandle { shutdown, task }) = handle else {
        return;
    };
    let _ = shutdown.send(true);
    let abort = task.abort_handle();
    let waited = match runtime {
        Some(rt) => rt
            .spawn(async move { tokio::time::timeout(STOP_BOUND, task).await.is_ok() })
            .await
            .unwrap_or(false),
        None => false,
    };
    if !waited {
        abort.abort();
        tracing::warn!(
            "port mapping: the gateway did not confirm every deletion within {}s; a mapping \
             left behind lapses when its lease runs out",
            STOP_BOUND.as_secs()
        );
    }
}

impl ReticulumNode {
    /// +ciris (leviculum#101): every mapping this run asked for, with its
    /// state, external address and lease. Empty while port mapping is off
    /// or before the first start.
    pub fn port_mappings(&self) -> Vec<PortMappingStatus> {
        self.port_mapping.lock().statuses.clone()
    }

    /// +ciris (leviculum#101): what the node knows about whether the
    /// internet can reach it, for a host (CIRISEdge) deciding what to
    /// advertise. [`ReachabilityReport::is_directly_reachable`] is the
    /// summary; it says `Verified` only after
    /// [`record_reachability_check`](Self::record_reachability_check)
    /// recorded a successful dial from outside.
    pub fn reachability(&self) -> ReachabilityReport {
        let shared = self.port_mapping.lock();
        let external_ipv4 = shared
            .statuses
            .iter()
            .filter(|s| s.state == MappingState::Mapped)
            .filter_map(|s| s.external)
            .map(|e| e.ip())
            .find(IpAddr::is_ipv4);
        let global_ipv6 = if shared.global_v6.is_empty() {
            crate::port_mapping::netinfo::global_ipv6_addresses()
        } else {
            shared.global_v6.clone()
        };
        ReachabilityReport {
            enabled: self.port_mapping.config.enabled,
            mappings: shared.statuses.clone(),
            external_ipv4,
            global_ipv6,
            cgnat_detected: shared
                .statuses
                .iter()
                .any(|s| matches!(s.state, MappingState::CgnatDetected { .. })),
            last_check: shared.last_check,
        }
    }

    /// +ciris (leviculum#101): record the result of a reachability check the
    /// host ran: something outside the router (a peer, or a server the host
    /// trusts) dialled `external` and it `answered` or not. The node cannot
    /// dial its own external address and learn anything (most routers loop
    /// it back inside), so only a host can run the check; until one does,
    /// a mapping is a claim, not proof.
    pub fn record_reachability_check(&self, external: SocketAddr, answered: bool) {
        tracing::debug!(
            "port mapping: a host check {} {external}",
            if answered {
                "reached this node at"
            } else {
                "could not reach this node at"
            }
        );
        self.port_mapping.lock().last_check = Some((external, answered, std::time::Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(kind: &str, ip: Option<&str>, port: Option<u16>) -> InterfaceConfig {
        InterfaceConfig {
            name: format!("{kind}-{port:?}"),
            interface_type: kind.to_string(),
            enabled: true,
            listen_ip: ip.map(str::to_string),
            listen_port: port,
            ..Default::default()
        }
    }

    /// The switch: on by default (as far as the environment allows), off
    /// through the builder or the config key, and the builder wins.
    #[tokio::test]
    async fn the_switch_turns_it_off_from_the_builder_or_the_config() {
        let build = |b: ReticulumNodeBuilder| async move {
            let dir = tempfile::tempdir().unwrap();
            let node = b
                .storage_path(dir.path().to_path_buf())
                .build()
                .await
                .unwrap();
            node.port_mapping.config.enabled
        };
        assert_eq!(
            build(ReticulumNodeBuilder::new()).await,
            crate::port_mapping::default_on()
        );
        assert!(!build(ReticulumNodeBuilder::new().enable_port_mapping(false)).await);
        let off: crate::config::Config =
            toml::from_str("[reticulum]\nport_mapping = false\n").unwrap();
        assert!(!build(ReticulumNodeBuilder::new().config(off)).await);
        if cfg!(feature = "port-mapping") {
            assert!(build(ReticulumNodeBuilder::new().enable_port_mapping(true)).await);
        }
    }

    #[test]
    fn listeners_map_unless_bound_to_loopback() {
        let ifaces = vec![
            iface("TCPServerInterface", None, Some(4242)),
            iface("TCPServerInterface", Some("127.0.0.1"), Some(4243)),
            iface("WebSocketServerInterface", Some("0.0.0.0"), Some(8765)),
            iface("TCPServerInterface", Some("::"), Some(4244)),
            iface("TCPClientInterface", None, Some(4245)),
            iface("TCPServerInterface", None, Some(0)),
        ];
        let keys = listener_keys(&ifaces, true);
        let got: Vec<(u16, bool)> = keys.iter().map(|k| (k.internal_port, k.v6)).collect();
        assert_eq!(
            got,
            vec![(4242, false), (8765, false), (4244, false), (4244, true)]
        );
        assert!(keys.iter().all(|k| k.protocol == MappingProtocol::Tcp));
        let no_v6 = listener_keys(&ifaces, false);
        assert!(no_v6.iter().all(|k| !k.v6));
    }
}
