//! +ciris (leviculum#101): router port mapping, so a home node behind an
//! ordinary router can be reached directly.
//!
//! A node with port mapping on asks its router to forward each of its
//! listening ports to it: the TCP server and WebSocket server listeners, and
//! the direct-link facilitator's UDP port when it runs one. It tries PCP
//! (RFC 6887) first, then NAT-PMP (RFC 6886), then UPnP-IGD (v2 or v1,
//! whichever the router offers), renews each mapping before its lease runs
//! out, and deletes them on a clean stop.
//!
//! Opening a port on someone's router is a security decision, so the
//! feature is off unless the config (`[reticulum] port_mapping = yes`) or
//! the builder ([`ReticulumNodeBuilder::port_mapping`]) turns it on, and
//! every mapping made or removed is logged at info level.
//!
//! A mapping is not proof that the node is reachable: the router can be
//! behind a carrier-grade NAT (reported as [`MappingState::CgnatDetected`]
//! when its external address says so, and undetectable when it does not),
//! an ISP can filter inbound connections, and some routers accept a mapping
//! they never honour. [`ReachabilityReport::is_directly_reachable`] says
//! what the node knows, and [`ReticulumNode::record_reachability_check`]
//! lets a host that dialled the node from outside say it worked.
//!
//! The protocol clients are behind the `port-mapping` cargo feature (on by
//! default); these types are always compiled, so a build without the
//! feature still reads the config key and refuses `port_mapping = yes`.
//!
//! [`ReticulumNodeBuilder::port_mapping`]: crate::driver::ReticulumNodeBuilder::port_mapping
//! [`ReticulumNode::record_reachability_check`]: crate::driver::ReticulumNode::record_reachability_check

#[cfg(feature = "port-mapping")]
pub(crate) mod clients;
// Without the feature only the types the node shares are used.
#[cfg_attr(not(feature = "port-mapping"), allow(dead_code))]
pub(crate) mod engine;
#[cfg_attr(not(feature = "port-mapping"), allow(dead_code))]
pub mod netinfo;
#[cfg(test)]
mod tests;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// The transport protocol of a mapped port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MappingProtocol {
    /// TCP (the TCP and WebSocket server listeners).
    Tcp,
    /// UDP (the direct-link facilitator).
    Udp,
}

impl MappingProtocol {
    /// `"tcp"` or `"udp"`.
    pub fn as_str(self) -> &'static str {
        match self {
            MappingProtocol::Tcp => "tcp",
            MappingProtocol::Udp => "udp",
        }
    }
}

/// How a mapping is requested from the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MappingMethod {
    /// Port Control Protocol (RFC 6887), UDP 5351 on the gateway. Over IPv6
    /// it opens a firewall pinhole instead of a NAT mapping.
    Pcp,
    /// NAT Port Mapping Protocol (RFC 6886), PCP's IPv4-only predecessor.
    NatPmp,
    /// UPnP Internet Gateway Device (WANIPConnection v2 or v1, or
    /// WANPPPConnection), found by SSDP and driven over SOAP.
    UpnpIgd,
}

impl MappingMethod {
    /// `"pcp"`, `"nat-pmp"` or `"upnp-igd"`, as `NodeEvent::PortMappingChanged`
    /// carries it.
    pub fn as_str(self) -> &'static str {
        match self {
            MappingMethod::Pcp => "pcp",
            MappingMethod::NatPmp => "nat-pmp",
            MappingMethod::UpnpIgd => "upnp-igd",
        }
    }
}

/// Where one requested mapping stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingState {
    /// Not tried yet.
    Pending,
    /// The router holds a mapping; `external` says from where. Still not
    /// proof of reachability (see the module docs).
    Mapped,
    /// A gateway answered and refused, or a held mapping could not be
    /// renewed before its lease ran out. Retried with backoff.
    Failed {
        /// What went wrong, for the operator.
        reason: String,
    },
    /// The router mapped the port but its own external address is private
    /// or in the carrier-grade shared range: another NAT sits upstream and
    /// the mapping opens nothing the internet can reach. The mapping was
    /// deleted again; it is re-checked every half hour.
    CgnatDetected {
        /// The external address the router reported.
        external_ip: IpAddr,
        /// Which range it fell in.
        reason: &'static str,
    },
    /// No PCP, NAT-PMP or UPnP-IGD gateway answered (for an IPv6 entry: no
    /// IPv6 gateway, or it does not speak PCP).
    NoGateway,
    /// Deleted on a clean stop.
    Removed,
}

/// One requested mapping, as [`ReticulumNode::port_mappings`] reports it.
///
/// [`ReticulumNode::port_mappings`]: crate::driver::ReticulumNode::port_mappings
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMappingStatus {
    /// TCP or UDP.
    pub protocol: MappingProtocol,
    /// The node's own listening port.
    pub internal_port: u16,
    /// Whether this is an IPv6 firewall pinhole (PCP only) rather than an
    /// IPv4 NAT mapping.
    pub ipv6: bool,
    /// How the mapping was made, once it was.
    pub method: Option<MappingMethod>,
    /// The router-side address peers dial. Its port is the router's choice
    /// and may differ from `internal_port`; advertise this one.
    pub external: Option<SocketAddr>,
    /// The lease the router granted, in seconds; `0` is a permanent UPnP
    /// mapping (some IGDv1 routers grant no other kind).
    pub lease_secs: u32,
    /// When the lease runs out, if it does; renewed at half-life.
    pub lease_expires: Option<Instant>,
    /// Where the mapping stands.
    pub state: MappingState,
}

impl PortMappingStatus {
    /// Whether the router assigned a different external port than the
    /// internal one.
    pub fn port_differs(&self) -> bool {
        self.external
            .is_some_and(|ext| ext.port() != self.internal_port)
    }

    #[cfg_attr(not(feature = "port-mapping"), allow(dead_code))]
    fn new(protocol: MappingProtocol, internal_port: u16, ipv6: bool) -> Self {
        PortMappingStatus {
            protocol,
            internal_port,
            ipv6,
            method: None,
            external: None,
            lease_secs: 0,
            lease_expires: None,
            state: MappingState::Pending,
        }
    }
}

/// What the node knows about whether the internet can reach it, as
/// [`ReticulumNode::reachability`] reports it.
///
/// [`ReticulumNode::reachability`]: crate::driver::ReticulumNode::reachability
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachabilityReport {
    /// Whether port mapping is on for this node.
    pub enabled: bool,
    /// Every requested mapping.
    pub mappings: Vec<PortMappingStatus>,
    /// The router's public IPv4 address, from the first live mapping.
    pub external_ipv4: Option<IpAddr>,
    /// Global IPv6 addresses on this host. Such an address needs no mapping
    /// to be addressed; whether the router's firewall admits a connection
    /// is unknown unless a PCP pinhole was made (an IPv6 entry in
    /// `mappings`) or a host check says so.
    pub global_ipv6: Vec<Ipv6Addr>,
    /// A carrier-grade or double NAT was seen: the external address a
    /// router reported was not public.
    pub cgnat_detected: bool,
    /// The last result a host recorded through
    /// [`ReticulumNode::record_reachability_check`]: the address dialled,
    /// whether it answered, and when.
    ///
    /// [`ReticulumNode::record_reachability_check`]: crate::driver::ReticulumNode::record_reachability_check
    pub last_check: Option<(SocketAddr, bool, Instant)>,
}

/// The honest summary of a [`ReachabilityReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    /// A host dialled the node from outside and it answered.
    Verified,
    /// A router holds a mapping with a public external address. Likely
    /// reachable, but nothing has dialled in to prove it.
    Mapped,
    /// The host has a global IPv6 address and nothing contradicts it, but
    /// the router's IPv6 firewall may still refuse inbound connections.
    GlobalIpv6,
    /// Nothing suggests the node can be reached: no mapping, a
    /// carrier-grade NAT, or a failed check.
    Unlikely,
}

impl ReachabilityReport {
    /// The summary: [`Reachability::Verified`] only when a host check
    /// succeeded, and [`Reachability::Unlikely`] when the last check failed,
    /// whatever the mappings say.
    pub fn is_directly_reachable(&self) -> Reachability {
        match self.last_check {
            Some((_, true, _)) => return Reachability::Verified,
            Some((_, false, _)) => return Reachability::Unlikely,
            None => {}
        }
        let mapped = self.mappings.iter().any(|m| {
            m.state == MappingState::Mapped
                && m.external
                    .is_some_and(|e| netinfo::non_public_reason(e.ip()).is_none())
        });
        if mapped {
            Reachability::Mapped
        } else if !self.global_ipv6.is_empty() {
            Reachability::GlobalIpv6
        } else {
            Reachability::Unlikely
        }
    }
}

/// Port-mapping settings, from `[reticulum]` or
/// [`ReticulumNodeBuilder::port_mapping`].
///
/// [`ReticulumNodeBuilder::port_mapping`]: crate::driver::ReticulumNodeBuilder::port_mapping
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMappingConfig {
    /// Map anything at all. Default off.
    pub enabled: bool,
    /// Map the node's listeners: every TCP and WebSocket server interface
    /// not bound to loopback, and the direct-link facilitator's UDP port.
    /// Default on (when `enabled`).
    pub map_listeners: bool,
    /// Further ports to map, beyond the listeners.
    pub extra_ports: Vec<(MappingProtocol, u16)>,
    /// The gateway to ask over PCP and NAT-PMP, instead of the OS's default
    /// route. UPnP finds its gateway by SSDP either way.
    pub gateway: Option<IpAddr>,
    /// The methods to try, in order. Default PCP, NAT-PMP, UPnP-IGD.
    pub methods: Vec<MappingMethod>,
    /// The lease to ask for. Default two hours, the NAT-PMP recommendation.
    pub lease: Duration,
    /// Also ask the IPv6 gateway for a PCP firewall pinhole for each
    /// listener bound to an IPv6 address. Default on.
    pub ipv6_pinholes: bool,
}

impl Default for PortMappingConfig {
    fn default() -> Self {
        PortMappingConfig {
            enabled: false,
            map_listeners: true,
            extra_ports: Vec::new(),
            gateway: None,
            methods: vec![
                MappingMethod::Pcp,
                MappingMethod::NatPmp,
                MappingMethod::UpnpIgd,
            ],
            lease: Duration::from_secs(7200),
            ipv6_pinholes: true,
        }
    }
}

impl PortMappingConfig {
    /// Port mapping on, with every other setting at its default.
    pub fn enabled() -> Self {
        PortMappingConfig {
            enabled: true,
            ..Default::default()
        }
    }

    /// Read the `[reticulum]` keys. A word other than yes/no, or a gateway
    /// that is not an IP address, is a config error: either typo would
    /// silently change whether this node opens ports on the router.
    pub fn from_config(config: &crate::config::ReticulumConfig) -> Result<Self, Error> {
        let keys = &config.direct_link;
        let enabled = match &keys.port_mapping {
            None => false,
            Some(ConfigSwitch::Bool(b)) => *b,
            Some(ConfigSwitch::Word(w)) => match w.trim().to_ascii_lowercase().as_str() {
                "" | "no" | "false" | "off" | "0" => false,
                "yes" | "true" | "on" | "1" => true,
                other => {
                    return Err(Error::Config(format!(
                        "port_mapping must be yes or no, not {other:?}"
                    )))
                }
            },
        };
        let gateway = match keys.port_mapping_gateway.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(s) => Some(s.parse().map_err(|_| {
                Error::Config(format!(
                    "port_mapping_gateway must be an IP address, not {s:?}"
                ))
            })?),
        };
        Ok(PortMappingConfig {
            enabled,
            gateway,
            ..Default::default()
        })
    }

    /// Refuse a config this build cannot honour, or one that cannot work.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if !self.enabled {
            return Ok(());
        }
        if !cfg!(feature = "port-mapping") {
            return Err(Error::Config(
                "port_mapping = yes needs leviculum-std's `port-mapping` feature, \
                 which this build leaves out"
                    .to_string(),
            ));
        }
        if self.methods.is_empty() {
            return Err(Error::Config(
                "port_mapping: no mapping method to try".to_string(),
            ));
        }
        if self.lease < Duration::from_secs(60) || self.lease > Duration::from_secs(86_400) {
            return Err(Error::Config(format!(
                "port_mapping: lease {:?} must be between one minute and one day",
                self.lease
            )));
        }
        if let Some((_, 0)) = self.extra_ports.iter().find(|(_, p)| *p == 0) {
            return Err(Error::Config(
                "port_mapping: port 0 cannot be mapped".to_string(),
            ));
        }
        Ok(())
    }
}

/// A `[reticulum]` switch as written: a TOML boolean, or a word (INI and
/// TOML strings), checked by [`PortMappingConfig::from_config`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigSwitch {
    /// `port_mapping = true` in TOML.
    Bool(bool),
    /// `port_mapping = yes` in INI, or a TOML string.
    Word(String),
}

/// Read a `[reticulum]` INI key of port mapping; `false` when the key is
/// not one of them. Reached from `direct_link::apply_ini_key`.
pub(crate) fn apply_ini_key(
    config: &mut crate::config::ReticulumConfig,
    key: &str,
    value: &str,
) -> bool {
    match key {
        "port_mapping" => {
            config.direct_link.port_mapping = Some(ConfigSwitch::Word(value.trim().to_string()))
        }
        "port_mapping_gateway" => {
            config.direct_link.port_mapping_gateway = Some(value.trim().to_string())
        }
        _ => return false,
    }
    true
}
