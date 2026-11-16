//! Admission control for the TCP server interface (1190).
//!
//! On 2026-10-31 one address held 972 of 1 135 connections on the public
//! backbone's TCP server; almost none of them ever sent a byte. The daemon
//! ran out of file descriptors, its accept loop spun on `EMFILE` writing one
//! log line per failed accept, and no legitimate client could get in. The
//! reference has no defence either: its `ThreadingTCPServer`
//! (`reference/Reticulum/RNS/Interfaces/TCPInterface.py:561`) calls
//! `incoming_connection` per accepted socket, which hands it to a new
//! `TCPClientInterface` without counting (`TCPInterface.py:575-578`).
//!
//! Everything here is the medium's own business (interface isolation): the
//! core never learns that a connection was refused or closed, it only sees an
//! interface that came and went. No wire byte changes.
//!
//! The pieces are pure and clocked by the caller, so the policy is tested
//! without sockets: [`ConnTracker`] decides admission, silent timeouts and
//! eviction; [`AcceptBackoff`] paces the accept loop after an error; the
//! descriptor helpers raise and read `RLIMIT_NOFILE`.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

/// Default `max_connections_per_source`. A household behind one NAT address
/// runs a handful of Reticulum programs; 16 leaves room for a busy one and is
/// still a sixtieth of what one address held in the incident.
pub(crate) const DEFAULT_MAX_CONNECTIONS_PER_SOURCE: usize = 16;
/// Default total cap, `max_connections` on a TCP server. The backbone serves
/// about 180 clients; the cap is a ceiling against exhaustion, not a quota.
pub(crate) const DEFAULT_MAX_CONNECTIONS: usize = 4096;
/// Default `silent_timeout_secs`: two minutes for a client to deliver its
/// first frame.
pub(crate) const DEFAULT_SILENT_TIMEOUT_SECS: u64 = 120;
/// Default `silent_timeout_pressure_secs`.
pub(crate) const DEFAULT_SILENT_TIMEOUT_PRESSURE_SECS: u64 = 10;
/// Default `pressure_fraction`.
pub(crate) const DEFAULT_PRESSURE_FRACTION: f64 = 0.8;

/// Fewest free descriptors a server interface starts with.
pub(crate) const MIN_SERVER_FDS: u64 = 256;
/// Descriptors kept free of the total cap for everything else the daemon
/// opens (other interfaces, storage, the shared-instance socket).
pub(crate) const FD_RESERVE: u64 = 64;

/// Least time between two reports of the same capped source.
const CAP_REPORT_INTERVAL: Duration = Duration::from_secs(60);
/// Least time between two `TCP_SILENT_CLOSED` lines.
const SILENT_REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// First pause of the accept loop after an error, doubled per error.
const ACCEPT_BACKOFF_FIRST: Duration = Duration::from_millis(10);
/// Longest pause of the accept loop.
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);
/// Least time between two `TCP_ACCEPT_BACKOFF` lines.
const ACCEPT_REPORT_INTERVAL: Duration = Duration::from_secs(10);

/// The resolved admission settings of one TCP server.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AdmissionConfig {
    /// Per source; `0` is off.
    pub max_connections_per_source: usize,
    /// Total; `0` is off.
    pub max_connections: usize,
    /// `Duration::ZERO` never closes a silent connection.
    pub silent_timeout: Duration,
    pub silent_timeout_pressure: Duration,
    pub pressure_fraction: f64,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            max_connections_per_source: DEFAULT_MAX_CONNECTIONS_PER_SOURCE,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            silent_timeout: Duration::from_secs(DEFAULT_SILENT_TIMEOUT_SECS),
            silent_timeout_pressure: Duration::from_secs(DEFAULT_SILENT_TIMEOUT_PRESSURE_SECS),
            pressure_fraction: DEFAULT_PRESSURE_FRACTION,
        }
    }
}

/// What one source is: an IPv4 address, or the /64 an IPv6 address sits in
/// (one subscriber's prefix; an address inside it costs nothing to change).
/// An IPv4-mapped IPv6 peer, which a dual-stack listener reports, is its
/// IPv4 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SourceKey {
    V4(Ipv4Addr),
    V6Prefix(u64),
}

impl From<IpAddr> for SourceKey {
    fn from(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => SourceKey::V4(v4),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => SourceKey::V4(v4),
                None => SourceKey::V6Prefix((u128::from(v6) >> 64) as u64),
            },
        }
    }
}

impl SourceKey {
    /// The address a log line tags this source by: the IPv4 address, or the
    /// /64 prefix with its host half zero. Every host in one /64 is one
    /// source, so they share a tag (order 1678).
    pub(crate) fn ip(&self) -> IpAddr {
        match self {
            SourceKey::V4(v4) => IpAddr::V4(*v4),
            SourceKey::V6Prefix(p) => IpAddr::V6(Ipv6Addr::from(u128::from(*p) << 64)),
        }
    }
}

impl fmt::Display for SourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceKey::V4(v4) => write!(f, "{v4}"),
            SourceKey::V6Prefix(p) => write!(f, "{}/64", Ipv6Addr::from(u128::from(*p) << 64)),
        }
    }
}

#[derive(Debug)]
struct Conn {
    source: SourceKey,
    peer: IpAddr,
    accepted: Instant,
    spoken: bool,
    close: Option<oneshot::Sender<()>>,
}

/// Why a new connection was closed right after accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Its source holds `max_connections_per_source`.
    Source,
    /// The server holds `max_connections` and no silent connection could be
    /// evicted for it.
    Total,
}

/// A connection the tracker let go of: closed as silent, or evicted to make
/// room. Sending on `close` tells its task to end.
#[derive(Debug)]
pub(crate) struct Closed {
    pub peer: IpAddr,
    pub after: Duration,
    pub reason: &'static str,
    close: Option<oneshot::Sender<()>>,
}

impl Closed {
    /// Tell the connection's task to end.
    pub(crate) fn close(mut self) {
        if let Some(tx) = self.close.take() {
            let _ = tx.send(());
        }
    }
}

/// The tracker's answer to a new connection.
#[derive(Debug)]
pub(crate) enum Admission {
    Accept {
        id: u64,
        close: oneshot::Receiver<()>,
        /// The connection closed to make room, under the total cap.
        evicted: Option<Closed>,
    },
    Refuse {
        reason: Refusal,
        source: SourceKey,
        /// Connections the source (for `Source`) or the server (for `Total`)
        /// holds.
        held: usize,
        /// Whether to report it: once per source per minute, and for the
        /// total cap once per minute.
        report: bool,
    },
}

/// The counters `lnstatus` shows for a TCP server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AdmissionStats {
    /// Connections held now.
    pub connections: usize,
    /// Of those, the ones that have not delivered a frame yet.
    pub silent: usize,
    /// Connections refused by either cap since start.
    pub capped: u64,
    /// Connections closed as silent (timeout or eviction) since start.
    pub silent_closed: u64,
}

/// Per-server connection bookkeeping (see the module docs).
#[derive(Debug)]
pub(crate) struct ConnTracker {
    config: AdmissionConfig,
    next_id: u64,
    /// By id, which is accept order: the first entry is the oldest.
    conns: BTreeMap<u64, Conn>,
    per_source: HashMap<SourceKey, usize>,
    capped: u64,
    silent_closed: u64,
    /// When each capped source was last reported, pruned after a minute.
    cap_reported: HashMap<SourceKey, Instant>,
    total_reported: Option<Instant>,
    silent_reported: Option<Instant>,
    silent_suppressed: u64,
}

impl ConnTracker {
    pub(crate) fn new(config: AdmissionConfig) -> Self {
        Self {
            config,
            next_id: 0,
            conns: BTreeMap::new(),
            per_source: HashMap::new(),
            capped: 0,
            silent_closed: 0,
            cap_reported: HashMap::new(),
            total_reported: None,
            silent_reported: None,
            silent_suppressed: 0,
        }
    }

    /// Decide on a connection just accepted from `peer`.
    pub(crate) fn admit(&mut self, peer: IpAddr, now: Instant) -> Admission {
        let source = SourceKey::from(peer);
        let held = self.per_source.get(&source).copied().unwrap_or(0);
        let per_source_cap = self.config.max_connections_per_source;
        if per_source_cap > 0 && held >= per_source_cap {
            self.capped += 1;
            let report = match self.cap_reported.get(&source) {
                Some(last) if now.duration_since(*last) < CAP_REPORT_INTERVAL => false,
                _ => {
                    self.cap_reported.insert(source, now);
                    true
                }
            };
            return Admission::Refuse {
                reason: Refusal::Source,
                source,
                held,
                report,
            };
        }
        let mut evicted = None;
        let total_cap = self.config.max_connections;
        if total_cap > 0 && self.conns.len() >= total_cap {
            match self.eviction_victim() {
                Some(victim) => evicted = self.release(victim, now, "evicted"),
                None => {
                    self.capped += 1;
                    let report = match self.total_reported {
                        Some(last) if now.duration_since(last) < CAP_REPORT_INTERVAL => false,
                        _ => {
                            self.total_reported = Some(now);
                            true
                        }
                    };
                    return Admission::Refuse {
                        reason: Refusal::Total,
                        source,
                        held: self.conns.len(),
                        report,
                    };
                }
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.conns.insert(
            id,
            Conn {
                source,
                peer,
                accepted: now,
                spoken: false,
                close: Some(tx),
            },
        );
        *self.per_source.entry(source).or_insert(0) += 1;
        Admission::Accept {
            id,
            close: rx,
            evicted,
        }
    }

    /// The connection delivered its first valid frame: it is never closed as
    /// silent from now on.
    pub(crate) fn mark_spoken(&mut self, id: u64) {
        if let Some(c) = self.conns.get_mut(&id) {
            c.spoken = true;
        }
    }

    /// The connection's task ended. Idempotent.
    pub(crate) fn remove(&mut self, id: u64) {
        if let Some(c) = self.conns.remove(&id) {
            self.drop_source(c.source);
        }
    }

    fn drop_source(&mut self, source: SourceKey) {
        if let Some(n) = self.per_source.get_mut(&source) {
            *n -= 1;
            if *n == 0 {
                self.per_source.remove(&source);
            }
        }
    }

    fn release(&mut self, id: u64, now: Instant, reason: &'static str) -> Option<Closed> {
        let mut c = self.conns.remove(&id)?;
        self.drop_source(c.source);
        self.silent_closed += 1;
        Some(Closed {
            peer: c.peer,
            after: now.saturating_duration_since(c.accepted),
            reason,
            close: c.close.take(),
        })
    }

    /// Whether the server holds more than `pressure_fraction` of its total
    /// cap. A server without a total cap is never under pressure.
    pub(crate) fn under_pressure(&self) -> bool {
        let cap = self.config.max_connections;
        cap > 0 && self.conns.len() as f64 > self.config.pressure_fraction * cap as f64
    }

    /// The silent timeout in force now.
    pub(crate) fn silent_timeout(&self) -> Duration {
        if self.under_pressure() {
            self.config.silent_timeout_pressure
        } else {
            self.config.silent_timeout
        }
    }

    /// The silent connection to close for a newcomer at the total cap: the
    /// oldest silent connection of the source that holds the most
    /// connections (ties go to the source whose silent connection is
    /// oldest). `None` when every connection has spoken.
    pub(crate) fn eviction_victim(&self) -> Option<u64> {
        let mut oldest_silent: HashMap<SourceKey, u64> = HashMap::new();
        for (&id, c) in &self.conns {
            if !c.spoken {
                oldest_silent.entry(c.source).or_insert(id);
            }
        }
        oldest_silent
            .into_iter()
            .max_by(|(sa, ia), (sb, ib)| {
                let na = self.per_source.get(sa).copied().unwrap_or(0);
                let nb = self.per_source.get(sb).copied().unwrap_or(0);
                na.cmp(&nb).then(ib.cmp(ia))
            })
            .map(|(_, id)| id)
    }

    /// Release every silent connection older than the silent timeout in
    /// force, and forget cap reports older than a minute.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<Closed> {
        self.cap_reported
            .retain(|_, last| now.duration_since(*last) < CAP_REPORT_INTERVAL);
        let timeout = self.silent_timeout();
        if timeout.is_zero() {
            return Vec::new();
        }
        let due: Vec<u64> = self
            .conns
            .iter()
            .filter(|(_, c)| !c.spoken && now.saturating_duration_since(c.accepted) >= timeout)
            .map(|(&id, _)| id)
            .collect();
        due.into_iter()
            .filter_map(|id| self.release(id, now, "timeout"))
            .collect()
    }

    /// Whether to log this silent close, at most once per ten seconds; the
    /// closes in between are counted and returned with the next one.
    pub(crate) fn silent_report(&mut self, now: Instant) -> Option<u64> {
        match self.silent_reported {
            Some(last) if now.duration_since(last) < SILENT_REPORT_INTERVAL => {
                self.silent_suppressed += 1;
                None
            }
            _ => {
                self.silent_reported = Some(now);
                Some(std::mem::take(&mut self.silent_suppressed))
            }
        }
    }

    pub(crate) fn stats(&self) -> AdmissionStats {
        AdmissionStats {
            connections: self.conns.len(),
            silent: self.conns.values().filter(|c| !c.spoken).count(),
            capped: self.capped,
            silent_closed: self.silent_closed,
        }
    }
}

/// The accept loop's pause after an error: 10 ms, doubling to 1 s, back to
/// the start after an accept succeeds; and one report per ten seconds that
/// counts the errors since the last one. Before 1190 the loop retried at
/// once, which on `EMFILE` is a busy loop: the connection that could not be
/// accepted stays queued, so the next accept fails the same way.
#[derive(Debug)]
pub(crate) struct AcceptBackoff {
    next: Duration,
    errors: u64,
    last_report: Option<Instant>,
}

impl Default for AcceptBackoff {
    fn default() -> Self {
        Self {
            next: ACCEPT_BACKOFF_FIRST,
            errors: 0,
            last_report: None,
        }
    }
}

impl AcceptBackoff {
    /// An accept failed: how long to pause, and the error count to report if
    /// a report is due.
    pub(crate) fn on_error(&mut self, now: Instant) -> (Duration, Option<u64>) {
        let pause = self.next;
        self.next = (self.next * 2).min(ACCEPT_BACKOFF_MAX);
        self.errors += 1;
        let report = match self.last_report {
            Some(last) if now.duration_since(last) < ACCEPT_REPORT_INTERVAL => None,
            _ => {
                self.last_report = Some(now);
                Some(std::mem::take(&mut self.errors))
            }
        };
        (pause, report)
    }

    /// An accept succeeded: the next error pauses briefly again.
    pub(crate) fn on_success(&mut self) {
        self.next = ACCEPT_BACKOFF_FIRST;
    }
}

/// The process's descriptor limit after [`raise_nofile_limit`], soft and
/// hard, and the soft limit it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NofileLimit {
    pub soft: u64,
    pub hard: u64,
    pub raised_from: u64,
}

/// Raise the soft `RLIMIT_NOFILE` to the hard limit, once per process, and
/// report the result. Systemd and most shells hand a daemon a soft limit of
/// 1024 under a much higher hard one; a server that holds one descriptor
/// per client should use what it is allowed. `None` where the limit cannot be
/// read (and on targets other than Linux and Android, which this leaves
/// alone).
pub(crate) fn raise_nofile_limit() -> Option<NofileLimit> {
    static RAISED: std::sync::OnceLock<Option<NofileLimit>> = std::sync::OnceLock::new();
    *RAISED.get_or_init(|| {
        let found = raise_nofile_limit_now();
        if let Some(l) = found {
            tracing::info!(
                event = "FD_LIMIT",
                soft = l.soft,
                hard = l.hard,
                raised_from = l.raised_from,
            );
        }
        found
    })
}

/// `rlim_t` as `u64`: the same type on 64-bit Linux, narrower on 32-bit
/// Android, so the cast is needed on one and redundant on the other.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(clippy::unnecessary_cast)]
fn rlim(v: libc::rlim_t) -> u64 {
    v as u64
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn raise_nofile_limit_now() -> Option<NofileLimit> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes into the struct we own.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return None;
    }
    let raised_from = rlim(lim.rlim_cur);
    // An unlimited hard limit is not a value the kernel takes for the soft
    // one (it is bounded by fs.nr_open); 2^20 is the usual nr_open.
    let target = if lim.rlim_max == libc::RLIM_INFINITY {
        1u64 << 20
    } else {
        rlim(lim.rlim_max)
    };
    if target > raised_from {
        let want = libc::rlimit {
            rlim_cur: target as libc::rlim_t,
            rlim_max: lim.rlim_max,
        };
        // SAFETY: setrlimit reads the struct we own.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &want) } == 0 {
            lim.rlim_cur = want.rlim_cur;
        } else {
            tracing::warn!(
                "could not raise the descriptor limit from {} to {}: {}",
                raised_from,
                target,
                std::io::Error::last_os_error()
            );
        }
    }
    Some(NofileLimit {
        soft: rlim(lim.rlim_cur),
        hard: rlim(lim.rlim_max),
        raised_from,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn raise_nofile_limit_now() -> Option<NofileLimit> {
    None
}

/// The soft descriptor limit in force now.
pub(crate) fn nofile_soft_limit() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes into the struct we own.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } == 0 {
            return Some(rlim(lim.rlim_cur));
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        None
    }
}

/// Descriptors this process holds now, where `/proc` says.
pub(crate) fn open_fds() -> Option<u64> {
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|d| d.count().saturating_sub(1) as u64)
}

/// `used/limit` for a log line, `?` for a part this host cannot tell.
/// Counting `/proc/self/fd` takes a descriptor itself, so on a full table the
/// count fails with `EMFILE`, which says the table is full: used is the
/// limit.
pub(crate) fn fd_usage() -> String {
    let limit = nofile_soft_limit();
    let used = match std::fs::read_dir("/proc/self/fd") {
        Ok(d) => Some(d.count().saturating_sub(1) as u64),
        Err(e) if e.raw_os_error() == Some(libc::EMFILE) => limit,
        Err(_) => None,
    };
    let show = |v: Option<u64>| v.map_or_else(|| "?".to_string(), |n| n.to_string());
    format!("{}/{}", show(used), show(limit))
}

/// The total cap a server can actually reach: the configured one, lowered to
/// the descriptors free at start minus [`FD_RESERVE`]. Past that the accept
/// loop would meet `EMFILE` before the cap, and the cap's eviction would
/// never run. `Err` with the free count when fewer than [`MIN_SERVER_FDS`]
/// are free.
pub(crate) fn effective_total_cap(
    configured: usize,
    soft_limit: Option<u64>,
    open: Option<u64>,
) -> Result<usize, u64> {
    let (Some(limit), Some(open)) = (soft_limit, open) else {
        return Ok(configured);
    };
    let free = limit.saturating_sub(open);
    if free < MIN_SERVER_FDS {
        return Err(free);
    }
    let reachable = (free - FD_RESERVE) as usize;
    Ok(match configured {
        0 => reachable,
        n => n.min(reachable),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(per_source: usize, total: usize) -> AdmissionConfig {
        AdmissionConfig {
            max_connections_per_source: per_source,
            max_connections: total,
            silent_timeout: Duration::from_secs(120),
            silent_timeout_pressure: Duration::from_secs(10),
            pressure_fraction: 0.8,
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn accept_id(a: Admission) -> u64 {
        match a {
            Admission::Accept { id, .. } => id,
            other => panic!("expected accept, got {other:?}"),
        }
    }

    #[test]
    fn ipv6_sources_are_their_64() {
        let a = SourceKey::from(ip("2001:db8:1:2::1"));
        let b = SourceKey::from(ip("2001:db8:1:2:ffff:ffff:ffff:ffff"));
        let c = SourceKey::from(ip("2001:db8:1:3::1"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.to_string(), "2001:db8:1:2::/64");
        assert_eq!(
            SourceKey::from(ip("::ffff:192.0.2.7")),
            SourceKey::from(ip("192.0.2.7"))
        );
    }

    #[test]
    fn a_source_at_its_cap_is_refused_and_reported_once_a_minute() {
        let mut t = ConnTracker::new(cfg(2, 0));
        let now = Instant::now();
        accept_id(t.admit(ip("192.0.2.1"), now));
        accept_id(t.admit(ip("192.0.2.1"), now));
        match t.admit(ip("192.0.2.1"), now) {
            Admission::Refuse {
                reason: Refusal::Source,
                held: 2,
                report: true,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            t.admit(ip("192.0.2.1"), now + Duration::from_secs(59)),
            Admission::Refuse { report: false, .. }
        ));
        // Another source is untouched.
        accept_id(t.admit(ip("192.0.2.2"), now));
        t.expire(now + Duration::from_secs(61));
        assert!(matches!(
            t.admit(ip("192.0.2.1"), now + Duration::from_secs(61)),
            Admission::Refuse { report: true, .. }
        ));
        assert_eq!(t.stats().capped, 3);
    }

    #[test]
    fn ipv6_addresses_of_one_64_share_the_cap() {
        let mut t = ConnTracker::new(cfg(2, 0));
        let now = Instant::now();
        accept_id(t.admit(ip("2001:db8::1"), now));
        accept_id(t.admit(ip("2001:db8::2"), now));
        assert!(matches!(
            t.admit(ip("2001:db8::3"), now),
            Admission::Refuse {
                reason: Refusal::Source,
                ..
            }
        ));
        accept_id(t.admit(ip("2001:db8:0:1::1"), now));
    }

    #[test]
    fn removing_a_connection_frees_its_source_slot() {
        let mut t = ConnTracker::new(cfg(1, 0));
        let now = Instant::now();
        let id = accept_id(t.admit(ip("192.0.2.1"), now));
        t.remove(id);
        t.remove(id);
        accept_id(t.admit(ip("192.0.2.1"), now));
        assert_eq!(t.stats().connections, 1);
    }

    #[test]
    fn a_zero_cap_is_off() {
        let mut t = ConnTracker::new(cfg(0, 0));
        let now = Instant::now();
        for _ in 0..100 {
            accept_id(t.admit(ip("192.0.2.1"), now));
        }
    }

    #[test]
    fn silent_timeout_closes_only_silent_connections() {
        let mut t = ConnTracker::new(cfg(0, 0));
        let now = Instant::now();
        let quiet = accept_id(t.admit(ip("192.0.2.1"), now));
        let talker = accept_id(t.admit(ip("192.0.2.2"), now));
        t.mark_spoken(talker);
        assert!(t.expire(now + Duration::from_secs(119)).is_empty());
        let closed = t.expire(now + Duration::from_secs(120));
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].peer, ip("192.0.2.1"));
        assert_eq!(closed[0].after, Duration::from_secs(120));
        assert!(t.expire(now + Duration::from_secs(10_000)).is_empty());
        let s = t.stats();
        assert_eq!((s.connections, s.silent, s.silent_closed), (1, 0, 1));
        t.remove(quiet);
        assert_eq!(t.stats().connections, 1);
    }

    #[test]
    fn under_pressure_the_silent_timeout_drops() {
        // Cap 10, pressure above 8.
        let mut t = ConnTracker::new(cfg(0, 10));
        let now = Instant::now();
        for i in 0..8 {
            accept_id(t.admit(ip(&format!("192.0.2.{i}")), now));
        }
        assert!(!t.under_pressure());
        assert_eq!(t.silent_timeout(), Duration::from_secs(120));
        assert!(t.expire(now + Duration::from_secs(10)).is_empty());
        accept_id(t.admit(ip("192.0.2.100"), now));
        assert!(t.under_pressure());
        assert_eq!(t.silent_timeout(), Duration::from_secs(10));
        assert_eq!(t.expire(now + Duration::from_secs(10)).len(), 9);
    }

    #[test]
    fn a_zero_silent_timeout_never_closes() {
        let mut c = cfg(0, 0);
        c.silent_timeout = Duration::ZERO;
        let mut t = ConnTracker::new(c);
        let now = Instant::now();
        accept_id(t.admit(ip("192.0.2.1"), now));
        assert!(t.expire(now + Duration::from_secs(100_000)).is_empty());
    }

    #[test]
    fn at_the_total_cap_the_oldest_silent_of_the_biggest_source_makes_room() {
        let mut t = ConnTracker::new(cfg(0, 6));
        let now = Instant::now();
        let a_spoke = accept_id(t.admit(ip("192.0.2.1"), now));
        t.mark_spoken(a_spoke);
        let a_old = accept_id(t.admit(ip("192.0.2.1"), now));
        let a_new = accept_id(t.admit(ip("192.0.2.1"), now));
        let b_old = accept_id(t.admit(ip("192.0.2.2"), now));
        let _b_new = accept_id(t.admit(ip("192.0.2.2"), now));
        let c = accept_id(t.admit(ip("192.0.2.3"), now));
        t.mark_spoken(c);
        assert!(b_old > a_old);
        assert_eq!(t.eviction_victim(), Some(a_old));
        match t.admit(ip("192.0.2.9"), now + Duration::from_secs(3)) {
            Admission::Accept {
                evicted: Some(e), ..
            } => {
                assert_eq!(e.peer, ip("192.0.2.1"));
                assert_eq!(e.after, Duration::from_secs(3));
                assert_eq!(e.reason, "evicted");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(t.stats().connections, 6);
        // Sources tie at two now: the older silent connection goes.
        assert!(a_new < b_old);
        assert_eq!(t.eviction_victim(), Some(a_new));
    }

    #[test]
    fn at_the_total_cap_with_everyone_spoken_the_newcomer_is_refused() {
        let mut t = ConnTracker::new(cfg(0, 2));
        let now = Instant::now();
        for i in 0..2 {
            let id = accept_id(t.admit(ip(&format!("192.0.2.{i}")), now));
            t.mark_spoken(id);
        }
        match t.admit(ip("192.0.2.9"), now) {
            Admission::Refuse {
                reason: Refusal::Total,
                held: 2,
                report: true,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            t.admit(ip("192.0.2.9"), now),
            Admission::Refuse { report: false, .. }
        ));
    }

    #[test]
    fn an_evicted_connection_is_told_to_close() {
        let mut t = ConnTracker::new(cfg(0, 1));
        let now = Instant::now();
        let mut first = match t.admit(ip("192.0.2.1"), now) {
            Admission::Accept { close, .. } => close,
            other => panic!("{other:?}"),
        };
        match t.admit(ip("192.0.2.2"), now) {
            Admission::Accept {
                evicted: Some(e), ..
            } => e.close(),
            other => panic!("{other:?}"),
        }
        assert_eq!(first.try_recv(), Ok(()));
    }

    #[test]
    fn silent_reports_are_one_per_ten_seconds() {
        let mut t = ConnTracker::new(cfg(0, 0));
        let now = Instant::now();
        assert_eq!(t.silent_report(now), Some(0));
        assert_eq!(t.silent_report(now + Duration::from_secs(1)), None);
        assert_eq!(t.silent_report(now + Duration::from_secs(2)), None);
        assert_eq!(t.silent_report(now + Duration::from_secs(10)), Some(2));
    }

    #[test]
    fn accept_backoff_doubles_to_a_second_and_resets_on_success() {
        let mut b = AcceptBackoff::default();
        let now = Instant::now();
        let pauses: Vec<u64> = (0..10)
            .map(|_| b.on_error(now).0.as_millis() as u64)
            .collect();
        assert_eq!(
            pauses,
            vec![10, 20, 40, 80, 160, 320, 640, 1000, 1000, 1000]
        );
        b.on_success();
        assert_eq!(b.on_error(now).0, Duration::from_millis(10));
    }

    #[test]
    fn accept_backoff_reports_once_per_ten_seconds_with_the_count() {
        let mut b = AcceptBackoff::default();
        let now = Instant::now();
        assert_eq!(b.on_error(now).1, Some(1));
        for i in 1..=5 {
            assert_eq!(b.on_error(now + Duration::from_secs(i)).1, None);
        }
        assert_eq!(b.on_error(now + Duration::from_secs(10)).1, Some(6));
    }

    #[test]
    fn the_total_cap_is_lowered_to_the_free_descriptors() {
        assert_eq!(effective_total_cap(4096, Some(1024), Some(24)), Ok(936));
        assert_eq!(effective_total_cap(100, Some(65536), Some(24)), Ok(100));
        assert_eq!(effective_total_cap(0, Some(1024), Some(24)), Ok(936));
        assert_eq!(effective_total_cap(4096, Some(200), Some(10)), Err(190));
        assert_eq!(effective_total_cap(4096, None, Some(10)), Ok(4096));
    }
}
