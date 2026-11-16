//! +ciris (leviculum#101): the port-mapping state machine. It holds one
//! entry per requested port, asks the protocol clients for each in order,
//! renews at half-life, retries with backoff, and deletes everything when
//! told to stop.
//!
//! The protocols sit behind [`MappingClient`], and gateway discovery behind
//! [`Discover`], so the whole schedule is tested against mocks on tokio's
//! paused clock.

use std::future::Future;
use std::net::{Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use super::{netinfo, MappingMethod, MappingProtocol, MappingState, PortMappingStatus};
use crate::sync_ext::MutexRecover;

pub(crate) type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Renew no sooner than this after a grant, whatever the lease.
const MIN_RENEW: Duration = Duration::from_secs(30);
/// First retry after a failure; doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(60);
/// The longest wait between attempts at a port that keeps failing.
const MAX_BACKOFF: Duration = Duration::from_secs(3600);
/// How often a CGNAT verdict is looked at again (the home may move, or the
/// ISP may hand out a public address). Long: a mapping that cannot help is
/// reported once, not asked for over and over.
const CGNAT_RECHECK: Duration = Duration::from_secs(6 * 3600);
/// How often a permanent (lease 0) UPnP mapping is re-added, so a router
/// that rebooted and forgot it gets it back.
const PERMANENT_RECHECK: Duration = Duration::from_secs(1200);
/// The bound on one request to a gateway. The clients have their own
/// retries; this catches an HTTP exchange that hangs.
pub(crate) const OP_TIMEOUT: Duration = Duration::from_secs(8);

/// One requested mapping's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PortKey {
    pub protocol: MappingProtocol,
    pub internal_port: u16,
    /// An IPv6 pinhole rather than an IPv4 mapping.
    pub v6: bool,
}

/// What a client is asked for.
#[derive(Debug, Clone)]
pub(crate) struct MapRequest {
    pub key: PortKey,
    pub lease_secs: u32,
    /// The external port to ask for: the one held, on a renewal, else the
    /// internal port. The router may assign another.
    pub external_port: u16,
}

/// What a router granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lease {
    pub external: SocketAddr,
    /// Seconds; `0` is permanent.
    pub lifetime_secs: u32,
}

/// Why a client produced no mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MapError {
    /// The gateway does not speak this method (no answer, wrong version, no
    /// such service): the next method is tried.
    Unsupported(String),
    /// It speaks it and said no.
    Refused(String),
}

/// One protocol toward one gateway.
pub(crate) trait MappingClient: Send + Sync {
    fn method(&self) -> MappingMethod;
    /// Whether it serves IPv6 pinholes rather than IPv4 mappings.
    fn v6(&self) -> bool;
    /// Make the mapping, or renew it when the client already holds it.
    fn map<'a>(&'a self, req: &'a MapRequest) -> BoxFut<'a, Result<Lease, MapError>>;
    /// Delete a mapping this client made.
    fn unmap<'a>(&'a self, key: PortKey, lease: &'a Lease) -> BoxFut<'a, Result<(), MapError>>;
}

/// The clients to try, built from what the host's network looks like now.
pub(crate) struct Discovered {
    pub clients: Vec<Arc<dyn MappingClient>>,
    pub global_v6: Vec<Ipv6Addr>,
}

/// Finds the gateways. Asked again whenever an unmapped entry is due, so a
/// laptop that changed networks is looked at afresh.
pub(crate) trait Discover: Send + Sync {
    fn discover(&self) -> BoxFut<'_, Discovered>;
}

/// What the engine shares with the node: the statuses and the IPv6 facts.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub statuses: Vec<PortMappingStatus>,
    pub global_v6: Vec<Ipv6Addr>,
    pub last_check: Option<(SocketAddr, bool, std::time::Instant)>,
}

/// Called with an entry whenever its external address appears, changes or
/// goes away.
pub(crate) type ChangeSink = Box<dyn Fn(&PortMappingStatus) + Send + Sync>;

struct Entry {
    key: PortKey,
    status: PortMappingStatus,
    client: Option<Arc<dyn MappingClient>>,
    lease: Option<Lease>,
    /// When the held lease runs out (`None` while unmapped or permanent).
    expires: Option<Instant>,
    next: Instant,
    failures: u32,
}

/// Info the first time, debug on every retry: a node with port mapping on by
/// default must stay quiet on a network that never answers.
macro_rules! first_info {
    ($first:expr, $($arg:tt)*) => {
        if $first {
            tracing::info!($($arg)*)
        } else {
            tracing::debug!($($arg)*)
        }
    };
}

pub(crate) struct Engine {
    stop: Option<watch::Receiver<bool>>,
    discover: Box<dyn Discover>,
    entries: Vec<Entry>,
    lease_secs: u32,
    shared: Arc<Mutex<Shared>>,
    sink: ChangeSink,
}

fn backoff(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    FIRST_BACKOFF.saturating_mul(1u32 << shift).min(MAX_BACKOFF)
}

/// When to renew a lease of `secs`: at half-life, as RFC 6886 asks.
fn renew_after(secs: u32) -> Duration {
    if secs == 0 {
        PERMANENT_RECHECK
    } else {
        (Duration::from_secs(u64::from(secs)) / 2).max(MIN_RENEW)
    }
}

impl Engine {
    pub(crate) fn new(
        keys: Vec<PortKey>,
        lease_secs: u32,
        discover: Box<dyn Discover>,
        shared: Arc<Mutex<Shared>>,
        sink: ChangeSink,
    ) -> Self {
        let now = Instant::now();
        let entries: Vec<Entry> = keys
            .into_iter()
            .map(|key| Entry {
                key,
                status: PortMappingStatus::new(key.protocol, key.internal_port, key.v6),
                client: None,
                lease: None,
                expires: None,
                next: now,
                failures: 0,
            })
            .collect();
        let engine = Engine {
            stop: None,
            discover,
            entries,
            lease_secs,
            shared,
            sink,
        };
        engine.publish();
        engine
    }

    fn publish(&self) {
        let mut shared = self.shared.lock_recover();
        shared.statuses = self.entries.iter().map(|e| e.status.clone()).collect();
    }

    /// Run until `shutdown` turns true (or its sender goes), then delete
    /// every held mapping.
    pub(crate) async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        self.stop = Some(shutdown.clone());
        loop {
            if *shutdown.borrow() {
                break;
            }
            self.step().await;
            let Some(next) = self.entries.iter().map(|e| e.next).min() else {
                // Nothing to map: wait for the stop.
                let _ = shutdown.changed().await;
                break;
            };
            tokio::select! {
                _ = tokio::time::sleep_until(next) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        self.release_all().await;
    }

    /// A stop was asked for: no new request goes out, so a stop waits for
    /// at most the one in flight.
    fn stopping(&self) -> bool {
        self.stop.as_ref().is_some_and(|rx| *rx.borrow())
    }

    /// Work every entry that is due.
    pub(crate) async fn step(&mut self) {
        let now = Instant::now();
        let mut discovered: Option<Discovered> = None;
        // Methods that went unanswered this round, so the next port does not
        // wait out the same timeout.
        let mut silent: Vec<(MappingMethod, bool)> = Vec::new();
        for i in 0..self.entries.len() {
            if self.stopping() {
                break;
            }
            if self.entries[i].next > now {
                continue;
            }
            if self.entries[i].lease.is_some() {
                self.renew(i).await;
            } else {
                if discovered.is_none() {
                    let d = self.discover.discover().await;
                    self.shared.lock_recover().global_v6 = d.global_v6.clone();
                    discovered = Some(d);
                }
                let clients = discovered
                    .as_ref()
                    .map(|d| d.clients.clone())
                    .unwrap_or_default();
                self.acquire(i, &clients, &mut silent).await;
            }
        }
        self.publish();
    }

    async fn acquire(
        &mut self,
        i: usize,
        clients: &[Arc<dyn MappingClient>],
        silent: &mut Vec<(MappingMethod, bool)>,
    ) {
        let key = self.entries[i].key;
        let req = MapRequest {
            key,
            lease_secs: self.lease_secs,
            external_port: key.internal_port,
        };
        let mut refusals: Vec<String> = Vec::new();
        let first = self.entries[i].failures == 0;
        for client in clients.iter().filter(|c| c.v6() == key.v6) {
            let tag = (client.method(), client.v6());
            if silent.contains(&tag) {
                continue;
            }
            if self.stopping() {
                return;
            }
            first_info!(
                first,
                "port mapping: asking the gateway over {} to open {} port {}{} to this host",
                client.method().as_str(),
                key.protocol.as_str(),
                key.internal_port,
                if key.v6 { " (IPv6 pinhole)" } else { "" },
            );
            match timed(client.map(&req)).await {
                Ok(lease) => {
                    self.granted(i, client.clone(), lease).await;
                    return;
                }
                Err(MapError::Unsupported(why)) => {
                    tracing::debug!(
                        "port mapping: {} unavailable: {why}",
                        client.method().as_str()
                    );
                    silent.push(tag);
                }
                Err(MapError::Refused(why)) => {
                    if first {
                        tracing::warn!(
                            "port mapping: {} refused {} port {}: {why}",
                            client.method().as_str(),
                            key.protocol.as_str(),
                            key.internal_port
                        );
                    }
                    tracing::debug!(
                        "port mapping: {} refused {} port {}: {why}",
                        client.method().as_str(),
                        key.protocol.as_str(),
                        key.internal_port
                    );
                    refusals.push(format!("{}: {why}", client.method().as_str()));
                }
            }
        }
        let e = &mut self.entries[i];
        e.failures += 1;
        e.next = Instant::now() + backoff(e.failures);
        e.status.state = if refusals.is_empty() {
            MappingState::NoGateway
        } else {
            MappingState::Failed {
                reason: refusals.join("; "),
            }
        };
        if e.failures == 1 && refusals.is_empty() {
            tracing::info!(
                "port mapping: no gateway answered for {} port {}{}; retrying later",
                key.protocol.as_str(),
                key.internal_port,
                if key.v6 { " (IPv6)" } else { "" },
            );
        }
    }

    /// A client granted a mapping: keep it, unless the router's own
    /// external address shows another NAT upstream.
    async fn granted(&mut self, i: usize, client: Arc<dyn MappingClient>, lease: Lease) {
        let key = self.entries[i].key;
        if let Some(reason) = netinfo::non_public_reason(lease.external.ip()) {
            // The address itself goes at debug only: the host decides what
            // to export (CIRISEdge, #101).
            let seen_before = matches!(
                self.entries[i].status.state,
                MappingState::CgnatDetected { .. }
            );
            first_info!(
                !seen_before,
                "port mapping: the gateway's external address is {reason}; another NAT \
                 sits upstream, so {} port {} cannot be reached from the internet and the \
                 mapping is deleted again (looked at again in {} h)",
                key.protocol.as_str(),
                key.internal_port,
                CGNAT_RECHECK.as_secs() / 3600
            );
            tracing::debug!(
                "port mapping: the gateway reported external address {}",
                lease.external.ip()
            );
            if let Err(e) = timed(client.unmap(key, &lease)).await {
                tracing::warn!("port mapping: could not delete the useless mapping: {e:?}");
            }
            let e = &mut self.entries[i];
            e.failures = 0;
            e.next = Instant::now() + CGNAT_RECHECK;
            e.status.state = MappingState::CgnatDetected {
                external_ip: lease.external.ip(),
                reason,
            };
            e.status.method = Some(client.method());
            return;
        }
        tracing::info!(
            "port mapping: mapped {} port {} via {} (lease {}); peers on the internet can \
             open connections to it",
            key.protocol.as_str(),
            key.internal_port,
            client.method().as_str(),
            lease_text(lease.lifetime_secs),
        );
        tracing::debug!(
            "port mapping: {} port {} is reachable at {}",
            key.protocol.as_str(),
            key.internal_port,
            lease.external
        );
        if lease.external.port() != key.internal_port {
            tracing::debug!(
                "port mapping: the gateway chose external port {} for internal port {}",
                lease.external.port(),
                key.internal_port
            );
        }
        if lease.lifetime_secs == 0 {
            tracing::warn!(
                "port mapping: the gateway grants only permanent UPnP mappings; {} port {} \
                 stays open if this node stops uncleanly",
                key.protocol.as_str(),
                key.internal_port
            );
        }
        self.hold(i, client, lease);
    }

    fn hold(&mut self, i: usize, client: Arc<dyn MappingClient>, lease: Lease) {
        let now = Instant::now();
        let e = &mut self.entries[i];
        let changed =
            e.status.external != Some(lease.external) || e.status.state != MappingState::Mapped;
        e.failures = 0;
        e.next = now + renew_after(lease.lifetime_secs);
        e.expires = (lease.lifetime_secs > 0)
            .then(|| now + Duration::from_secs(u64::from(lease.lifetime_secs)));
        e.status.method = Some(client.method());
        e.status.external = Some(lease.external);
        e.status.lease_secs = lease.lifetime_secs;
        e.status.lease_expires = e.expires.map(Instant::into_std);
        e.status.state = MappingState::Mapped;
        e.client = Some(client);
        e.lease = Some(lease);
        if changed {
            (self.sink)(&e.status);
        }
    }

    async fn renew(&mut self, i: usize) {
        let (Some(client), Some(lease)) = (
            self.entries[i].client.clone(),
            self.entries[i].lease.clone(),
        ) else {
            return;
        };
        let key = self.entries[i].key;
        let req = MapRequest {
            key,
            lease_secs: self.lease_secs,
            external_port: lease.external.port(),
        };
        match timed(client.map(&req)).await {
            Ok(new) => {
                if new.external != lease.external {
                    tracing::debug!(
                        "port mapping: the gateway moved {} port {} from {} to {}",
                        key.protocol.as_str(),
                        key.internal_port,
                        lease.external,
                        new.external
                    );
                }
                if netinfo::non_public_reason(new.external.ip()).is_some() {
                    // The router's WAN side changed under us.
                    self.lose(i, "the gateway's external address is no longer public");
                    let _ = timed(client.unmap(key, &new)).await;
                    return;
                }
                self.hold(i, client, new);
            }
            Err(err) => {
                let reason = match err {
                    MapError::Unsupported(w) | MapError::Refused(w) => w,
                };
                let now = Instant::now();
                match self.entries[i].expires {
                    Some(exp) if now < exp => {
                        let left = exp - now;
                        tracing::warn!(
                            "port mapping: renewing {} port {} failed ({reason}); {}s of \
                             lease left, retrying",
                            key.protocol.as_str(),
                            key.internal_port,
                            left.as_secs()
                        );
                        self.entries[i].next =
                            now + (left / 2).clamp(Duration::from_secs(5), FIRST_BACKOFF);
                    }
                    _ => {
                        self.lose(i, &format!("renewal failed: {reason}"));
                    }
                }
            }
        }
    }

    /// The held mapping is gone (or never was useful): report it lost and
    /// start over with backoff.
    fn lose(&mut self, i: usize, reason: &str) {
        let e = &mut self.entries[i];
        tracing::warn!(
            "port mapping: lost {} port {}: {reason}",
            e.key.protocol.as_str(),
            e.key.internal_port
        );
        e.client = None;
        e.lease = None;
        e.expires = None;
        e.failures += 1;
        e.next = Instant::now() + backoff(e.failures);
        e.status.external = None;
        e.status.lease_secs = 0;
        e.status.lease_expires = None;
        e.status.state = MappingState::Failed {
            reason: reason.to_string(),
        };
        (self.sink)(&e.status);
    }

    /// Delete every held mapping (the clean stop).
    pub(crate) async fn release_all(&mut self) {
        for e in &mut self.entries {
            let (Some(client), Some(lease)) = (e.client.take(), e.lease.take()) else {
                continue;
            };
            match timed(client.unmap(e.key, &lease)).await {
                Ok(()) => tracing::info!(
                    "port mapping: removed the mapping of {} port {} from the gateway",
                    e.key.protocol.as_str(),
                    e.key.internal_port
                ),
                Err(err) => tracing::warn!(
                    "port mapping: could not remove the mapping of {} port {} from the \
                     gateway ({err:?}); it lapses {}",
                    e.key.protocol.as_str(),
                    e.key.internal_port,
                    if lease.lifetime_secs == 0 {
                        "never: delete it on the router by hand".to_string()
                    } else {
                        "when its lease runs out".to_string()
                    }
                ),
            }
            e.expires = None;
            e.status.external = None;
            e.status.lease_expires = None;
            e.status.state = MappingState::Removed;
            (self.sink)(&e.status);
        }
        self.publish();
    }
}

fn lease_text(secs: u32) -> String {
    if secs == 0 {
        "permanent".to_string()
    } else {
        format!("{secs}s")
    }
}

/// Bound one client call by [`OP_TIMEOUT`].
async fn timed<T>(fut: BoxFut<'_, Result<T, MapError>>) -> Result<T, MapError> {
    match tokio::time::timeout(OP_TIMEOUT, fut).await {
        Ok(r) => r,
        Err(_) => Err(MapError::Unsupported(format!(
            "no answer within {}s",
            OP_TIMEOUT.as_secs()
        ))),
    }
}

#[cfg(test)]
pub(crate) fn backoff_for_test(failures: u32) -> Duration {
    backoff(failures)
}

#[cfg(test)]
pub(crate) fn renew_after_for_test(secs: u32) -> Duration {
    renew_after(secs)
}
