//! +ciris (leviculum#101): the protocol clients behind [`MappingClient`]:
//! PCP and NAT-PMP through `crab_nat`, UPnP-IGD through `igd-next`, and the
//! [`Discover`] that builds them from the host's routing table.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU16;
use std::sync::Mutex;
use std::time::Duration;

use crab_nat::{natpmp, pcp, GatewayAddress, InternetProtocol, PortMappingOptions, TimeoutConfig};
use igd_next::aio::tokio::{search_gateway, Tokio};
use igd_next::aio::Gateway;
use igd_next::{
    AddAnyPortError, AddPortError, GetGenericPortMappingEntryError as GenericError,
    PortMappingProtocol, SearchOptions,
};

use super::engine::{
    BoxFut, Discover, Discovered, Lease, MapError, MapRequest, MappingClient, PortKey,
};
use super::{netinfo, MappingMethod, MappingProtocol, PortMappingConfig};
use crate::sync_ext::MutexRecover;

/// How many entries of the router's UPnP table the start-up reclaim reads
/// at most.
const RECLAIM_MAX_ENTRIES: u32 = 128;
/// The bound on reading the table, whatever its size.
const RECLAIM_BOUND: Duration = Duration::from_secs(5);

/// PCP and NAT-PMP retries: 250 ms, 500 ms, 1 s, so a gateway that does
/// not speak the protocol costs under two seconds rather than the RFC's
/// minute.
const PMP_TIMEOUTS: TimeoutConfig = TimeoutConfig {
    initial_timeout: Duration::from_millis(250),
    max_retries: 2,
    max_retry_timeout: Some(Duration::from_secs(1)),
};

fn proto(p: MappingProtocol) -> InternetProtocol {
    match p {
        MappingProtocol::Tcp => InternetProtocol::Tcp,
        MappingProtocol::Udp => InternetProtocol::Udp,
    }
}

fn nz(port: u16) -> Result<NonZeroU16, MapError> {
    NonZeroU16::new(port).ok_or_else(|| MapError::Refused("port 0 cannot be mapped".into()))
}

fn pcp_err(e: pcp::Failure) -> MapError {
    match e {
        pcp::Failure::Timeout | pcp::Failure::Socket(_) | pcp::Failure::UnsupportedVersion(_) => {
            MapError::Unsupported(format!("PCP: {e}"))
        }
        other => MapError::Refused(format!("PCP: {other}")),
    }
}

fn pmp_err(e: natpmp::Failure) -> MapError {
    match e {
        natpmp::Failure::Timeout
        | natpmp::Failure::Socket(_)
        | natpmp::Failure::UnsupportedVersion(_) => MapError::Unsupported(format!("NAT-PMP: {e}")),
        other => MapError::Refused(format!("NAT-PMP: {other}")),
    }
}

/// PCP toward one gateway, IPv4 (a NAT mapping) or IPv6 (a firewall
/// pinhole). Holds each mapping so a renewal reuses its nonce, as RFC 6887
/// requires.
pub(crate) struct PcpClient {
    gateway: GatewayAddress,
    client_ip: IpAddr,
    held: Mutex<HashMap<PortKey, crab_nat::PortMapping>>,
}

impl PcpClient {
    pub(crate) fn new(gateway: GatewayAddress, client_ip: IpAddr) -> Self {
        PcpClient {
            gateway,
            client_ip,
            held: Mutex::new(HashMap::new()),
        }
    }
}

impl MappingClient for PcpClient {
    fn method(&self) -> MappingMethod {
        MappingMethod::Pcp
    }

    fn v6(&self) -> bool {
        self.client_ip.is_ipv6()
    }

    fn map<'a>(&'a self, req: &'a MapRequest) -> BoxFut<'a, Result<Lease, MapError>> {
        Box::pin(async move {
            let held = self.held.lock_recover().get(&req.key).cloned();
            let mapping = match held {
                Some(mut m) => {
                    m.renew().await.map_err(|e| match e {
                        crab_nat::MappingFailure::Pcp(e) => pcp_err(e),
                        crab_nat::MappingFailure::NatPmp(e) => pmp_err(e),
                    })?;
                    m
                }
                None => pcp::port_mapping(
                    pcp::BaseMapRequest::new(
                        self.gateway,
                        self.client_ip,
                        proto(req.key.protocol),
                        nz(req.key.internal_port)?,
                    ),
                    None,
                    None,
                    PortMappingOptions {
                        external_port: NonZeroU16::new(req.external_port),
                        lifetime_seconds: Some(req.lease_secs.max(1)),
                        timeout_config: Some(PMP_TIMEOUTS),
                    },
                )
                .await
                .map_err(pcp_err)?,
            };
            let crab_nat::PortMappingType::Pcp { external_ip, .. } = mapping.mapping_type() else {
                return Err(MapError::Refused("PCP answered as NAT-PMP".into()));
            };
            let lease = Lease {
                external: SocketAddr::new(external_ip, mapping.external_port().get()),
                lifetime_secs: mapping.lifetime().max(1),
            };
            self.held.lock_recover().insert(req.key, mapping);
            Ok(lease)
        })
    }

    fn unmap<'a>(&'a self, key: PortKey, _lease: &'a Lease) -> BoxFut<'a, Result<(), MapError>> {
        Box::pin(async move {
            let Some(m) = self.held.lock_recover().remove(&key) else {
                return Ok(());
            };
            m.try_drop()
                .await
                .map_err(|(e, _)| MapError::Refused(e.to_string()))
        })
    }
}

/// NAT-PMP toward one IPv4 gateway.
pub(crate) struct NatPmpClient {
    gateway: GatewayAddress,
    held: Mutex<HashMap<PortKey, crab_nat::PortMapping>>,
}

impl NatPmpClient {
    pub(crate) fn new(gateway: Ipv4Addr) -> Self {
        NatPmpClient {
            gateway: GatewayAddress::IpV4(gateway),
            held: Mutex::new(HashMap::new()),
        }
    }
}

impl MappingClient for NatPmpClient {
    fn method(&self) -> MappingMethod {
        MappingMethod::NatPmp
    }

    fn v6(&self) -> bool {
        false
    }

    fn map<'a>(&'a self, req: &'a MapRequest) -> BoxFut<'a, Result<Lease, MapError>> {
        Box::pin(async move {
            let held = self.held.lock_recover().get(&req.key).cloned();
            let mapping = match held {
                Some(mut m) => {
                    m.renew().await.map_err(|e| match e {
                        crab_nat::MappingFailure::Pcp(e) => pcp_err(e),
                        crab_nat::MappingFailure::NatPmp(e) => pmp_err(e),
                    })?;
                    m
                }
                None => natpmp::port_mapping(
                    self.gateway,
                    proto(req.key.protocol),
                    nz(req.key.internal_port)?,
                    PortMappingOptions {
                        external_port: NonZeroU16::new(req.external_port),
                        lifetime_seconds: Some(req.lease_secs.max(1)),
                        timeout_config: Some(PMP_TIMEOUTS),
                    },
                )
                .await
                .map_err(pmp_err)?,
            };
            // A NAT-PMP mapping does not carry the address; ask for it.
            let external_ip = natpmp::external_address(self.gateway, Some(PMP_TIMEOUTS))
                .await
                .map_err(pmp_err)?;
            let lease = Lease {
                external: SocketAddr::new(IpAddr::V4(external_ip), mapping.external_port().get()),
                lifetime_secs: mapping.lifetime().max(1),
            };
            self.held.lock_recover().insert(req.key, mapping);
            Ok(lease)
        })
    }

    fn unmap<'a>(&'a self, key: PortKey, _lease: &'a Lease) -> BoxFut<'a, Result<(), MapError>> {
        Box::pin(async move {
            let Some(m) = self.held.lock_recover().remove(&key) else {
                return Ok(());
            };
            m.try_drop()
                .await
                .map_err(|(e, _)| MapError::Refused(e.to_string()))
        })
    }
}

/// UPnP-IGD, found by SSDP from this host's LAN address. igd-next takes the
/// WAN connection service the device offers (WANIPConnection v2 or v1, or
/// WANPPPConnection); `AddAnyPortMapping`, IGDv2's call for a router-chosen
/// port, is used when the internal port is taken.
pub(crate) struct UpnpClient {
    local_ip: Ipv4Addr,
    ssdp: SocketAddr,
    /// Addresses trusted as a device's location beyond the SSDP responder's
    /// own (igd-next refuses loopback ones otherwise; the tests need it).
    trusted: Vec<IpAddr>,
    gateway: tokio::sync::Mutex<Option<Gateway<Tokio>>>,
    /// This node's tag (`super::upnp_tag`): the description prefix that
    /// marks a mapping in the router's table as ours.
    tag: String,
    /// The (protocol, internal port) pairs this node maps over IPv4.
    ours: Vec<(MappingProtocol, u16)>,
    /// External ports of our own stale mappings found at start, to ask for
    /// again so a crash-restart keeps its external port.
    reclaimed: Mutex<HashMap<(MappingProtocol, u16), u16>>,
}

fn igd_proto(p: MappingProtocol) -> PortMappingProtocol {
    match p {
        MappingProtocol::Tcp => PortMappingProtocol::TCP,
        MappingProtocol::Udp => PortMappingProtocol::UDP,
    }
}

impl UpnpClient {
    /// Search from `local_ip` toward `ssdp` (the multicast group in
    /// production, a loopback responder in tests).
    pub(crate) fn new(
        local_ip: Ipv4Addr,
        ssdp: SocketAddr,
        tag: String,
        ours: Vec<(MappingProtocol, u16)>,
    ) -> Self {
        UpnpClient {
            local_ip,
            ssdp,
            trusted: Vec::new(),
            gateway: tokio::sync::Mutex::new(None),
            tag,
            ours,
            reclaimed: Mutex::new(HashMap::new()),
        }
    }

    /// `leviculum:<tag>:<proto>/<internal port>`: stable across runs, and
    /// free of any RNS identity or destination hash (the table is readable
    /// from the whole LAN).
    fn description(&self, protocol: MappingProtocol, internal_port: u16) -> String {
        format!(
            "leviculum:{}:{}/{internal_port}",
            self.tag,
            protocol.as_str()
        )
    }

    /// Find this node's own mappings left in the router's table by a run
    /// that did not stop cleanly (IGDv1 routers grant only permanent
    /// leases, so they never lapse). One is kept per port, to be asked for
    /// again; any further duplicate is deleted. A mapping is ours only when
    /// its description carries our tag, its internal client is this host's
    /// LAN address and its protocol and internal port are ones we map:
    /// nothing else, another leviculum node's included, is touched. A
    /// router that cannot list its table gets the plain path, which
    /// re-adds the same external port for the same client and so replaces
    /// the stale entry on most IGDs.
    async fn reclaim(&self, gw: &Gateway<Tokio>) {
        let prefix = format!("leviculum:{}:", self.tag);
        let mut found: Vec<(MappingProtocol, u16, u16)> = Vec::new();
        let listing = async {
            for index in 0..RECLAIM_MAX_ENTRIES {
                let entry = match gw.get_generic_port_mapping_entry(index).await {
                    Ok(e) => e,
                    Err(e) => {
                        if index == 0 && !matches!(e, GenericError::SpecifiedArrayIndexInvalid) {
                            tracing::debug!(
                                "port mapping: the UPnP gateway does not list its table ({e}); \
                                 re-adding the same external ports instead"
                            );
                        }
                        break;
                    }
                };
                let protocol = match entry.protocol {
                    PortMappingProtocol::TCP => MappingProtocol::Tcp,
                    PortMappingProtocol::UDP => MappingProtocol::Udp,
                };
                let client_is_us = entry
                    .internal_client
                    .trim()
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip == IpAddr::V4(self.local_ip));
                if entry.port_mapping_description.starts_with(&prefix)
                    && client_is_us
                    && self.ours.contains(&(protocol, entry.internal_port))
                {
                    found.push((protocol, entry.internal_port, entry.external_port));
                }
            }
        };
        if tokio::time::timeout(RECLAIM_BOUND, listing).await.is_err() {
            tracing::debug!("port mapping: reading the UPnP table took too long; stopped");
        }
        if found.is_empty() {
            return;
        }
        let (mut reused, mut removed) = (0usize, 0usize);
        let mut reclaimed = HashMap::new();
        for (protocol, internal, external) in found {
            if let std::collections::hash_map::Entry::Vacant(v) =
                reclaimed.entry((protocol, internal))
            {
                v.insert(external);
                reused += 1;
                tracing::debug!(
                    "port mapping: reusing our stale UPnP mapping of {} port {internal} \
                     (external port {external})",
                    protocol.as_str()
                );
            } else if gw.remove_port(igd_proto(protocol), external).await.is_ok() {
                removed += 1;
                tracing::debug!(
                    "port mapping: removed a duplicate stale UPnP mapping of {} port \
                     {internal} (external port {external})",
                    protocol.as_str()
                );
            }
        }
        tracing::info!(
            "port mapping: found this node's stale UPnP mappings from an earlier run: \
             {reused} reused, {removed} removed"
        );
        *self.reclaimed.lock_recover() = reclaimed;
    }

    /// Also trust a device location at `ip` (tests: a loopback IGD).
    #[cfg(test)]
    pub(crate) fn trusting(mut self, ip: IpAddr) -> Self {
        self.trusted.push(ip);
        self
    }

    async fn gateway(&self) -> Result<Gateway<Tokio>, MapError> {
        let mut cached = self.gateway.lock().await;
        if let Some(gw) = cached.as_ref() {
            return Ok(gw.clone());
        }
        let mut search = SearchOptions::default();
        search.bind_addr = SocketAddr::new(IpAddr::V4(self.local_ip), 0);
        search.broadcast_address = self.ssdp;
        search.timeout = Some(Duration::from_secs(3));
        search.single_search_timeout = Some(Duration::from_secs(2));
        search.allowed_gateway_ips = self.trusted.clone();
        let gw = search_gateway(search)
            .await
            .map_err(|e| MapError::Unsupported(format!("UPnP-IGD: {e}")))?;
        tracing::debug!(
            "port mapping: UPnP gateway at {} offers {}",
            gw.addr,
            gw.service_type
        );
        self.reclaim(&gw).await;
        *cached = Some(gw.clone());
        Ok(gw)
    }

    async fn add(
        &self,
        gw: &Gateway<Tokio>,
        protocol: PortMappingProtocol,
        external_port: u16,
        local: SocketAddr,
        lease: u32,
        desc: &str,
    ) -> Result<(u16, u32), MapError> {
        let refused = |e: &dyn std::fmt::Display| MapError::Refused(format!("UPnP-IGD: {e}"));
        match gw
            .add_port(protocol, external_port, local, lease, desc)
            .await
        {
            Ok(()) => return Ok((external_port, lease)),
            Err(AddPortError::OnlyPermanentLeasesSupported) if lease != 0 => {
                return match gw.add_port(protocol, external_port, local, 0, desc).await {
                    Ok(()) => Ok((external_port, 0)),
                    Err(e) => Err(refused(&e)),
                };
            }
            Err(AddPortError::PortInUse) => {}
            Err(AddPortError::RequestError(e)) => {
                return Err(MapError::Unsupported(format!("UPnP-IGD: {e}")));
            }
            Err(e) => return Err(refused(&e)),
        }
        // The external port is someone else's: let the router choose.
        match gw.add_any_port(protocol, local, lease, desc).await {
            Ok(port) => Ok((port, lease)),
            Err(AddAnyPortError::OnlyPermanentLeasesSupported) if lease != 0 => gw
                .add_any_port(protocol, local, 0, desc)
                .await
                .map(|port| (port, 0))
                .map_err(|e| refused(&e)),
            Err(e) => Err(refused(&e)),
        }
    }
}

impl MappingClient for UpnpClient {
    fn method(&self) -> MappingMethod {
        MappingMethod::UpnpIgd
    }

    fn v6(&self) -> bool {
        false
    }

    fn map<'a>(&'a self, req: &'a MapRequest) -> BoxFut<'a, Result<Lease, MapError>> {
        Box::pin(async move {
            let gw = self.gateway().await?;
            let protocol = match req.key.protocol {
                MappingProtocol::Tcp => PortMappingProtocol::TCP,
                MappingProtocol::Udp => PortMappingProtocol::UDP,
            };
            let local = SocketAddr::new(IpAddr::V4(self.local_ip), req.key.internal_port);
            let desc = self.description(req.key.protocol, req.key.internal_port);
            let stale = self
                .reclaimed
                .lock_recover()
                .remove(&(req.key.protocol, req.key.internal_port));
            let result = match stale {
                // Ask for our stale mapping's external port again: the same
                // client and port replace the entry, and the external port
                // survives the crash-restart.
                Some(ext) => match self
                    .add(&gw, protocol, ext, local, req.lease_secs, &desc)
                    .await
                {
                    Ok((port, lease)) => {
                        if port != ext {
                            // Replaced elsewhere: the stale entry is an orphan.
                            let _ = gw.remove_port(protocol, ext).await;
                        }
                        Ok((port, lease))
                    }
                    Err(_) => {
                        let _ = gw.remove_port(protocol, ext).await;
                        self.add(
                            &gw,
                            protocol,
                            req.external_port,
                            local,
                            req.lease_secs,
                            &desc,
                        )
                        .await
                    }
                },
                None => {
                    self.add(
                        &gw,
                        protocol,
                        req.external_port,
                        local,
                        req.lease_secs,
                        &desc,
                    )
                    .await
                }
            };
            let (port, lease) = match result {
                Ok(v) => v,
                Err(e) => {
                    // The router may have gone or changed; search again next time.
                    if matches!(e, MapError::Unsupported(_)) {
                        *self.gateway.lock().await = None;
                    }
                    return Err(e);
                }
            };
            let ip = gw
                .get_external_ip()
                .await
                .map_err(|e| MapError::Refused(format!("UPnP-IGD external address: {e}")))?;
            Ok(Lease {
                external: SocketAddr::new(ip, port),
                lifetime_secs: lease,
            })
        })
    }

    fn unmap<'a>(&'a self, key: PortKey, lease: &'a Lease) -> BoxFut<'a, Result<(), MapError>> {
        Box::pin(async move {
            let gw = self.gateway().await?;
            let protocol = match key.protocol {
                MappingProtocol::Tcp => PortMappingProtocol::TCP,
                MappingProtocol::Udp => PortMappingProtocol::UDP,
            };
            gw.remove_port(protocol, lease.external.port())
                .await
                .map_err(|e| MapError::Refused(format!("UPnP-IGD: {e}")))
        })
    }
}

/// The SSDP multicast group UPnP devices listen on.
const SSDP_V4: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 1900);

/// The gateway and local address the cached clients were built for.
type CachedClients = (Option<IpAddr>, Option<IpAddr>, Discovered);

/// Builds the clients from the OS's routes and the config, once per round
/// that has an unmapped entry. The clients are cached while the gateway
/// and local address stay the same, so a held PCP nonce survives.
pub(crate) struct SystemDiscover {
    config: PortMappingConfig,
    want_v6: bool,
    tag: String,
    ours: Vec<(MappingProtocol, u16)>,
    cache: Mutex<Option<CachedClients>>,
}

impl SystemDiscover {
    pub(crate) fn new(
        config: PortMappingConfig,
        want_v6: bool,
        tag: String,
        ours: Vec<(MappingProtocol, u16)>,
    ) -> Self {
        SystemDiscover {
            config,
            want_v6,
            tag,
            ours,
            cache: Mutex::new(None),
        }
    }
}

impl Discover for SystemDiscover {
    fn discover(&self) -> BoxFut<'_, Discovered> {
        Box::pin(async move {
            let config = self.config.clone();
            let want_v6 = self.want_v6;
            // Reading the routes may run a command (BSD, Windows): off the
            // async worker.
            let facts = tokio::task::spawn_blocking(move || {
                let gw4 = match config.gateway {
                    Some(IpAddr::V4(v4)) => Some(v4),
                    Some(IpAddr::V6(_)) => None,
                    None => netinfo::default_gateway_v4(),
                };
                let local4 = netinfo::local_address_toward(SocketAddr::new(
                    IpAddr::V4(gw4.unwrap_or(Ipv4Addr::new(192, 0, 2, 1))),
                    5351,
                ));
                let gw6 = if !want_v6 || !config.ipv6_pinholes {
                    None
                } else {
                    match config.gateway {
                        Some(IpAddr::V6(v6)) => Some((v6, 0)),
                        _ => netinfo::default_gateway_v6(),
                    }
                };
                // The source address the kernel picks for the internet,
                // which is the address a PCP pinhole has to name.
                let local6 = gw6.and_then(|_| {
                    netinfo::local_address_toward("[2001:4860:4860::8888]:5351".parse().ok()?)
                        .filter(|ip| matches!(ip, IpAddr::V6(v6) if netinfo::is_global_v6(*v6)))
                });
                (gw4, local4, gw6, local6, netinfo::global_ipv6_addresses())
            })
            .await
            .unwrap_or((None, None, None, None, Vec::new()));
            let (gw4, local4, gw6, local6, global_v6) = facts;

            let key = (gw4.map(IpAddr::V4), local4);
            {
                let cache = self.cache.lock_recover();
                if let Some((g, l, d)) = cache.as_ref() {
                    if (*g, *l) == key {
                        return Discovered {
                            clients: d.clients.clone(),
                            global_v6,
                        };
                    }
                }
            }
            let mut clients: Vec<std::sync::Arc<dyn MappingClient>> = Vec::new();
            for method in &self.config.methods {
                match method {
                    MappingMethod::Pcp => {
                        if let (Some(gw), Some(local @ IpAddr::V4(_))) = (gw4, local4) {
                            clients.push(std::sync::Arc::new(PcpClient::new(
                                GatewayAddress::IpV4(gw),
                                local,
                            )));
                        }
                        if let (Some((gw, scope)), Some(local)) = (gw6, local6) {
                            clients.push(std::sync::Arc::new(PcpClient::new(
                                GatewayAddress::IpV6(gw, (scope != 0).then_some(scope)),
                                local,
                            )));
                        }
                    }
                    MappingMethod::NatPmp => {
                        if let Some(gw) = gw4 {
                            clients.push(std::sync::Arc::new(NatPmpClient::new(gw)));
                        }
                    }
                    MappingMethod::UpnpIgd => {
                        if let Some(IpAddr::V4(local)) = local4 {
                            clients.push(std::sync::Arc::new(UpnpClient::new(
                                local,
                                SSDP_V4,
                                self.tag.clone(),
                                self.ours.clone(),
                            )));
                        }
                    }
                }
            }
            tracing::debug!(
                "port mapping: gateway {:?}, local {:?}, IPv6 gateway {:?}, {} client(s)",
                gw4,
                local4,
                gw6,
                clients.len()
            );
            let d = Discovered {
                clients: clients.clone(),
                global_v6: global_v6.clone(),
            };
            *self.cache.lock_recover() = Some((
                key.0,
                key.1,
                Discovered {
                    clients,
                    global_v6: Vec::new(),
                },
            ));
            d
        })
    }
}
