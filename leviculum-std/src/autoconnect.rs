//! Runtime auto-connect of discovered interfaces (Codeberg #32, sub-task b).
//!
//! Sub-task (c) persists validated discovery announces as
//! [`DiscoveredInterfaceRecord`]s under `<storage>/discovery/interfaces`. This
//! layer turns those records into live connections: when auto-connect is
//! enabled and a record for an auto-connectable endpoint appears, it spawns a
//! TCP client to the advertised `host:port` at runtime, registers it with the
//! running event loop, and tears it down when the backing record disappears or
//! the connection stays down past a detach threshold.
//!
//! # Scope (matches Python)
//!
//! Python `RNS.Discovery.InterfaceDiscovery` only *actually* auto-connects a
//! `BackboneInterface` (`autoconnect`, `Discovery.py:626-677`);
//! `TCPClientInterface`/`I2PInterface` auto-connect is explicitly left
//! unimplemented ("add manually via `rnstatus -D`"). A `BackboneInterface`
//! reaches its peer over the same TCP client transport as a
//! `TCPServerInterface` in our stack (Codeberg #89), so both advertised types
//! map to a runtime-spawned TCP client to the advertised endpoint. Other types
//! (I2P and anything not in [`AUTOCONNECT_TYPES`]) are logged as unimplemented
//! and skipped, mirroring Python.
//!
//! # Layering
//!
//! The lifecycle state machine ([`AutoConnectManager`]) is medium-agnostic: it
//! decides *when* to spawn and tear down, and drives those through an
//! [`AutoConnectSpawner`]. The production spawner (in the driver event loop)
//! owns the TCP-client-specific wiring (address resolution, the reconnecting
//! interface task, event-loop registration). This keeps the carrier specifics
//! in the interface layer and the runtime-management logic here, and makes the
//! spawn/register/teardown lifecycle unit-testable against a mock spawner.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use leviculum_core::crypto::full_hash;
use leviculum_core::discovery::DiscoveredInterfaceRecord;
use leviculum_core::transport::InterfaceId;

/// Interface types we auto-connect at runtime (Python
/// `InterfaceDiscovery.AUTOCONNECT_TYPES`). Both advertise a reachable
/// `host:port` and are reached over a TCP client in our stack.
pub(crate) const AUTOCONNECT_TYPES: [&str; 2] = ["BackboneInterface", "TCPServerInterface"];

/// How long an auto-connected interface may report offline before it is torn
/// down (Python `InterfaceDiscovery.DETACH_THRESHOLD`, in seconds).
pub(crate) const DETACH_THRESHOLD_SECS: f64 = 12.0;

/// Ceiling for the [cooldown](AutoConnectManager::cooldown) of an endpoint that
/// has never once connected, in seconds.
///
/// The cost of one retry is a full dial cycle: a TCP client task, an interface
/// registration, [`DETACH_THRESHOLD_SECS`] of a held slot, the core-side
/// teardown, and the connect-failure warnings. Capping at five minutes turns a
/// permanently unusable record from ~6 300 of those a day into ~276, while
/// keeping the delay before an endpoint that starts working is picked up
/// bounded by a figure an operator can wait out. Python has no equivalent
/// ceiling (see the deviation note on [`AutoConnectManager::cooldown`]).
const COOLDOWN_MAX_SECS: f64 = 300.0;

/// The spawn + teardown surface [`AutoConnectManager`] drives. Split out so the
/// lifecycle is unit-testable against a mock, while production wires it to the
/// running event loop's interface channels and online map.
pub(crate) trait AutoConnectSpawner {
    /// Spawn a TCP client interface to `host:port` at runtime and register it
    /// with the transport. `rec` is the backing discovery record; the spawner
    /// resolves the interface's IFAC from it (advertised netname/netkey, or
    /// inherited from the interface the announce was heard on — Codeberg #151).
    /// Returns the assigned [`InterfaceId`], or `None` if the endpoint could
    /// not be resolved, the interface could not be spawned, or the spawner
    /// refused the endpoint (fail closed: IFAC required but unresolvable).
    fn spawn_tcp_client(
        &mut self,
        name: &str,
        host: &str,
        port: u16,
        rec: &DiscoveredInterfaceRecord,
    ) -> Option<InterfaceId>;

    /// Tear down a previously spawned auto-connected interface.
    fn teardown(&mut self, id: InterfaceId);

    /// Whether the interface is currently online (its transport carrier is up).
    fn is_online(&self, id: InterfaceId) -> bool;
}

/// One live auto-connected interface, tracked for dedup and teardown.
struct Active {
    id: InterfaceId,
    /// `full_hash(reachable_on[:port])` — Python's `endpoint_hash`, the dedup
    /// key so the same endpoint is never auto-connected twice.
    endpoint_hash: [u8; 32],
    /// Wall-clock (Unix seconds) the interface was first seen offline, or
    /// `None` while it is online. Drives the detach-threshold teardown.
    down_since: Option<f64>,
    /// Whether this attachment ever reported online. An endpoint torn down
    /// without having done so once is a candidate for the cooldown; one that
    /// connected and later dropped is not.
    ever_online: bool,
}

/// The backoff carried by an endpoint that has been auto-connected and torn
/// down without ever coming online.
struct Cooldown {
    /// Wall-clock (Unix seconds) before which the endpoint takes no slot.
    until: f64,
    /// Consecutive dial cycles that never reached online, driving the doubling.
    consecutive: u32,
}

/// How long an endpoint that has never connected waits before it may take a
/// slot again: [`DETACH_THRESHOLD_SECS`] doubling per consecutive failed dial
/// cycle, capped at [`COOLDOWN_MAX_SECS`]. 12 s, 24 s, 48 s, ... 300 s.
fn cooldown_secs(consecutive: u32) -> f64 {
    let doublings = consecutive.saturating_sub(1).min(16);
    (DETACH_THRESHOLD_SECS * f64::from(1u32 << doublings)).min(COOLDOWN_MAX_SECS)
}

/// Runtime auto-connect lifecycle for discovered interfaces.
///
/// A single integer gates and bounds the feature (matching Python, where
/// `autoconnect_discovered_interfaces` is both the on/off flag and the cap):
/// `0` disables it, `N > 0` enables it with at most `N` concurrently
/// auto-connected interfaces.
pub(crate) struct AutoConnectManager {
    max_interfaces: usize,
    active: Vec<Active>,
    /// Endpoints (`discovery_hash`) already warned about as unimplemented, so
    /// the repeated poll does not spam the log.
    warned_unimplemented: BTreeSet<[u8; 32]>,
    /// Per-endpoint backoff for endpoints that have never once connected
    /// (Codeberg #414). Keyed by `endpoint_hash`, pruned every poll to the
    /// endpoints still backed by a live record, so it is bounded by the
    /// discovered set.
    ///
    /// DELIBERATE DEVIATION from Python, which has no such state: there, a
    /// discovered endpoint that can never be reached is re-dialled on the
    /// monitor tick forever. Permitted by the project deviation rule — a
    /// client's own dial cadence is invisible on the wire and to every peer,
    /// and the measured 22 dial cycles per five minutes per unusable record
    /// (#414) is wasted work and log volume on exactly the constrained
    /// backbone nodes Priority 1 is about. It is a backoff, never a blacklist:
    /// the endpoint keeps being retried, just at a rate that decays.
    cooldown: BTreeMap<[u8; 32], Cooldown>,
    /// Auto-connected interfaces reporting online as of the last
    /// [`poll`](Self::poll) — Python's `online_interfaces`
    /// (`Discovery.py:524-531`), counted after that poll's teardown pass so an
    /// interface on its way out is not counted as connectivity we have.
    online: usize,
}

/// What the `bootstrap_only` interfaces should do this tick
/// (Python `Discovery.py:553-563`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BootstrapAction {
    /// Leave them as they are.
    Keep,
    /// Enough auto-discovered interfaces are online: the seed connections have
    /// done their job and are detached.
    Detach,
    /// No auto-discovered interface is left and no bootstrap interface is
    /// attached: re-establish them, or the node has no way back in.
    ReAttach,
}

impl AutoConnectManager {
    /// Create a manager. `max_interfaces == 0` leaves auto-connect disabled.
    pub(crate) fn new(max_interfaces: usize) -> Self {
        Self {
            max_interfaces,
            active: Vec::new(),
            warned_unimplemented: BTreeSet::new(),
            cooldown: BTreeMap::new(),
            online: 0,
        }
    }

    /// The lifecycle step the `bootstrap_only` interfaces owe this tick, given
    /// how many of them are currently attached (Codeberg #416).
    ///
    /// Python compares its count of online auto-discovered interfaces against
    /// `max_autoconnected_interfaces` — the same single integer that enables
    /// auto-connect at all — and tears every bootstrap-only interface down once
    /// the target is reached; when the count falls back to zero and no
    /// bootstrap interface is left, it re-creates them from the saved configs.
    /// With auto-connect disabled there is nothing that could replace a seed
    /// connection, so the key stays inert, exactly as in Python (the monitor
    /// job only runs for auto-connected interfaces).
    pub(crate) fn bootstrap_action(&self, attached: usize) -> BootstrapAction {
        if !self.enabled() {
            return BootstrapAction::Keep;
        }
        if attached > 0 && self.online >= self.max_interfaces {
            BootstrapAction::Detach
        } else if attached == 0 && self.online == 0 {
            BootstrapAction::ReAttach
        } else {
            BootstrapAction::Keep
        }
    }

    /// Whether auto-connect is enabled.
    pub(crate) fn enabled(&self) -> bool {
        self.max_interfaces > 0
    }

    /// Number of currently auto-connected interfaces (Python
    /// `autoconnect_count`).
    #[cfg(test)]
    pub(crate) fn active_count(&self) -> usize {
        self.active.len()
    }

    /// The endpoint dedup key Python auto-connections are keyed by:
    /// `full_hash(reachable_on + optional(":" + port))` (`Discovery.py:601-606`).
    pub(crate) fn endpoint_hash(reachable_on: &str, port: Option<u64>) -> [u8; 32] {
        let mut spec = String::from(reachable_on);
        if let Some(p) = port {
            spec.push(':');
            spec.push_str(&p.to_string());
        }
        full_hash(spec.as_bytes())
    }

    /// Resolve a record to a `(host, port)` TCP connect target, or `None` if
    /// the record is not an auto-connectable type or lacks a usable endpoint.
    fn connect_target(rec: &DiscoveredInterfaceRecord) -> Option<(&str, u16)> {
        if !AUTOCONNECT_TYPES.contains(&rec.interface_type.as_str()) {
            return None;
        }
        let host = rec.reachable_on.as_deref()?;
        let port = u16::try_from(rec.port?).ok()?;
        Some((host, port))
    }

    /// Drop internal tracking for an interface removed out-of-band (the event
    /// loop saw `Disconnected` and removed it). A later [`poll`](Self::poll)
    /// may re-auto-connect if the endpoint is still discovered.
    pub(crate) fn on_interface_removed(&mut self, id: InterfaceId) {
        self.active.retain(|a| a.id != id);
    }

    /// Reconcile the live discovered set against currently auto-connected
    /// interfaces (Python `connect_discovered` + `__monitor_job`).
    ///
    /// Teardown pass: an auto-connected interface is torn down when its backing
    /// record is gone from `live` (record expiry/removal) or it has reported
    /// offline continuously for at least [`DETACH_THRESHOLD_SECS`] (connection
    /// death). Spawn pass: each transport-capable auto-connectable record with
    /// an endpoint we are not already connected to is spawned, up to the
    /// configured cap. `now` is wall-clock seconds (for the offline timer).
    pub(crate) fn poll(
        &mut self,
        live: &[DiscoveredInterfaceRecord],
        now: f64,
        spawner: &mut impl AutoConnectSpawner,
    ) {
        if !self.enabled() {
            return;
        }

        // Endpoints still backed by a live autoconnectable record.
        let live_endpoints: BTreeSet<[u8; 32]> = live
            .iter()
            .filter(|r| Self::connect_target(r).is_some())
            .map(|r| Self::endpoint_hash(r.reachable_on.as_deref().unwrap_or(""), r.port))
            .collect();

        // Teardown pass. Endpoints torn down this tick are not re-spawned in the
        // spawn pass below: for an offline (connection-death) teardown the
        // record is still live, and instantly reconnecting the just-detached
        // endpoint would thrash. A later poll may re-establish it.
        let mut torn_this_tick: BTreeSet<[u8; 32]> = BTreeSet::new();
        let mut i = 0;
        while i < self.active.len() {
            let endpoint_hash = self.active[i].endpoint_hash;
            let record_gone = !live_endpoints.contains(&endpoint_hash);
            // Distinguished from `record_gone` because only a connection that
            // died (or never came up) says anything about the endpoint; a
            // record that expired says only that its owner stopped announcing.
            let mut offline_detach = false;
            let detach = if record_gone {
                true
            } else if spawner.is_online(self.active[i].id) {
                self.active[i].down_since = None;
                if !self.active[i].ever_online {
                    self.active[i].ever_online = true;
                    // It connects: it owes nothing to the cooldown table, so a
                    // later outage gets Python's prompt re-attach.
                    self.cooldown.remove(&endpoint_hash);
                }
                false
            } else {
                match self.active[i].down_since {
                    None => {
                        self.active[i].down_since = Some(now);
                        false
                    }
                    Some(t) => {
                        offline_detach = now - t >= DETACH_THRESHOLD_SECS;
                        offline_detach
                    }
                }
            };

            if detach {
                let id = self.active[i].id;
                if offline_detach && !self.active[i].ever_online {
                    let entry = self.cooldown.entry(endpoint_hash).or_insert(Cooldown {
                        until: now,
                        consecutive: 0,
                    });
                    entry.consecutive = entry.consecutive.saturating_add(1);
                    entry.until = now + cooldown_secs(entry.consecutive);
                }
                torn_this_tick.insert(endpoint_hash);
                spawner.teardown(id);
                self.active.remove(i);
            } else {
                i += 1;
            }
        }

        // Bounded by the live discovered set: an endpoint nobody advertises any
        // more carries no backoff, and is dialled fresh if it is re-discovered.
        self.cooldown.retain(|k, _| live_endpoints.contains(k));

        // Connectivity we actually have, read after the teardown pass and
        // before the spawn pass: an interface just detached is gone, and one
        // dialled below has not connected yet. Both would otherwise make the
        // bootstrap decision on a link that carries nothing (Codeberg #416).
        self.online = self
            .active
            .iter()
            .filter(|a| spawner.is_online(a.id))
            .count();

        // Spawn pass. `live` is caller-sorted best-first (Python
        // list_discovered_interfaces order), so the cap keeps the best peers.
        for rec in live {
            if self.active.len() >= self.max_interfaces {
                break;
            }
            // Python lists candidates with only_transport=True.
            if !rec.transport {
                continue;
            }
            let Some((host, port)) = Self::connect_target(rec) else {
                self.maybe_warn_unimplemented(rec);
                continue;
            };
            let endpoint_hash = Self::endpoint_hash(host, rec.port);
            if torn_this_tick.contains(&endpoint_hash) {
                continue; // just detached this tick; do not immediately reconnect
            }
            if self
                .cooldown
                .get(&endpoint_hash)
                .is_some_and(|c| now < c.until)
            {
                // Never once connected: waiting out its backoff. `continue`,
                // not `break` — the slot it is not taking goes to the next
                // candidate rather than staying empty.
                continue;
            }
            if self.active.iter().any(|a| a.endpoint_hash == endpoint_hash) {
                continue; // already auto-connected to this endpoint
            }
            let name = format!("autoconnect/{}", rec.name);
            if let Some(id) = spawner.spawn_tcp_client(&name, host, port, rec) {
                tracing::info!(
                    "discovery: auto-connecting {} \"{}\" at {}:{}",
                    rec.interface_type,
                    rec.name,
                    host,
                    port
                );
                self.active.push(Active {
                    id,
                    endpoint_hash,
                    down_since: None,
                    ever_online: false,
                });
            }
        }
    }

    /// Log (once per endpoint) that a discovered type is recognised but its
    /// auto-connect is not implemented, mirroring Python's warning for I2P.
    fn maybe_warn_unimplemented(&mut self, rec: &DiscoveredInterfaceRecord) {
        if rec.interface_type == "I2PInterface"
            && self.warned_unimplemented.insert(rec.discovery_hash)
        {
            tracing::warn!(
                "discovery: auto-connecting discovered I2P interfaces is not yet implemented; \
                 obtain the config entry and add it manually via `lnstatus -D`"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::discovery::DiscoveredInterface;
    use leviculum_core::discovery::STAMP_SIZE;

    /// Records spawner calls so the lifecycle can be asserted deterministically.
    #[derive(Default)]
    struct MockSpawner {
        next_id: usize,
        spawned: Vec<(String, String, u16, InterfaceId)>,
        /// IFAC netname/netkey of the record each spawn was handed (#151).
        spawned_ifac: Vec<(Option<String>, Option<String>)>,
        torn_down: Vec<InterfaceId>,
        /// Interface ids currently reporting offline (default: online).
        offline: BTreeSet<usize>,
        /// If set, the next spawn returns `None` (resolution failure).
        fail_next_spawn: bool,
        /// Hosts whose spawned interfaces never report online, the way an
        /// unroutable endpoint (`::`, an IPv6 target with no route) behaves.
        never_online: BTreeSet<String>,
        /// Ids spawned towards a [`never_online`](Self::never_online) host.
        dead_ids: BTreeSet<usize>,
    }

    impl AutoConnectSpawner for MockSpawner {
        fn spawn_tcp_client(
            &mut self,
            name: &str,
            host: &str,
            port: u16,
            rec: &DiscoveredInterfaceRecord,
        ) -> Option<InterfaceId> {
            if self.fail_next_spawn {
                self.fail_next_spawn = false;
                return None;
            }
            let id = InterfaceId(self.next_id);
            self.next_id += 1;
            if self.never_online.contains(host) {
                self.dead_ids.insert(id.0);
            }
            self.spawned
                .push((name.to_string(), host.to_string(), port, id));
            self.spawned_ifac
                .push((rec.ifac_netname.clone(), rec.ifac_netkey.clone()));
            Some(id)
        }
        fn teardown(&mut self, id: InterfaceId) {
            self.torn_down.push(id);
        }
        fn is_online(&self, id: InterfaceId) -> bool {
            !self.offline.contains(&id.0) && !self.dead_ids.contains(&id.0)
        }
    }

    fn backbone_rec(name: &str, host: &str, port: u16, seed: u8) -> DiscoveredInterfaceRecord {
        let di = DiscoveredInterface {
            interface_type: "BackboneInterface".to_string(),
            transport: true,
            name: name.to_string(),
            transport_id: [seed; 16],
            network_id: [seed; 16],
            value: 20,
            stamp: [seed; STAMP_SIZE],
            latitude: None,
            longitude: None,
            height: None,
            reachable_on: Some(host.to_string()),
            port: Some(port as u64),
            frequency: None,
            bandwidth: None,
            spreadingfactor: None,
            codingrate: None,
            ifac_netname: None,
            ifac_netkey: None,
            discovery_hash: [seed; STAMP_SIZE],
        };
        DiscoveredInterfaceRecord::from_discovered(&di, 1, 1000.0, 1000.0, 1000.0, 0)
    }

    fn i2p_rec(seed: u8) -> DiscoveredInterfaceRecord {
        let di = DiscoveredInterface {
            interface_type: "I2PInterface".to_string(),
            transport: true,
            name: "i2p".to_string(),
            transport_id: [seed; 16],
            network_id: [seed; 16],
            value: 20,
            stamp: [seed; STAMP_SIZE],
            latitude: None,
            longitude: None,
            height: None,
            reachable_on: Some("abcd.b32.i2p".to_string()),
            port: None,
            frequency: None,
            bandwidth: None,
            spreadingfactor: None,
            codingrate: None,
            ifac_netname: None,
            ifac_netkey: None,
            discovery_hash: [seed; STAMP_SIZE],
        };
        DiscoveredInterfaceRecord::from_discovered(&di, 1, 1000.0, 1000.0, 1000.0, 0)
    }

    #[test]
    fn disabled_manager_never_spawns() {
        let mut mgr = AutoConnectManager::new(0);
        let mut sp = MockSpawner::default();
        assert!(!mgr.enabled());
        mgr.poll(&[backbone_rec("B", "10.0.0.5", 4965, 1)], 1000.0, &mut sp);
        assert!(sp.spawned.is_empty(), "disabled manager must not spawn");
    }

    #[test]
    fn discovered_record_spawns_and_registers_once() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        assert_eq!(sp.spawned.len(), 1, "one interface spawned + registered");
        assert_eq!(sp.spawned[0].1, "10.0.0.5");
        assert_eq!(sp.spawned[0].2, 4965);
        assert_eq!(mgr.active_count(), 1);

        // Re-polling the same record must not double-connect the endpoint.
        mgr.poll(std::slice::from_ref(&rec), 1001.0, &mut sp);
        assert_eq!(sp.spawned.len(), 1, "endpoint dedup prevents re-spawn");
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn record_removal_tears_down_interface() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        let id = sp.spawned[0].3;
        assert_eq!(mgr.active_count(), 1);

        // Record gone from the live set (expired/removed) -> teardown.
        mgr.poll(&[], 1100.0, &mut sp);
        assert_eq!(
            sp.torn_down,
            vec![id],
            "removed record unregisters interface"
        );
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn offline_past_threshold_tears_down() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        let id = sp.spawned[0].3;
        sp.offline.insert(id.0);

        // First offline poll only stamps down_since; no teardown yet.
        mgr.poll(std::slice::from_ref(&rec), 1001.0, &mut sp);
        assert!(sp.torn_down.is_empty(), "one offline poll must not detach");
        assert_eq!(mgr.active_count(), 1);

        // Still offline past the detach threshold -> teardown.
        mgr.poll(
            std::slice::from_ref(&rec),
            1001.0 + DETACH_THRESHOLD_SECS,
            &mut sp,
        );
        assert_eq!(sp.torn_down, vec![id], "sustained offline detaches");
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn recovered_before_threshold_is_not_torn_down() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        let id = sp.spawned[0].3;

        sp.offline.insert(id.0);
        mgr.poll(std::slice::from_ref(&rec), 1001.0, &mut sp); // stamp down
        sp.offline.remove(&id.0); // back online before threshold
        mgr.poll(
            std::slice::from_ref(&rec),
            1001.0 + DETACH_THRESHOLD_SECS,
            &mut sp,
        );
        assert!(sp.torn_down.is_empty(), "recovery clears the offline timer");
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn cap_bounds_concurrent_autoconnects() {
        let mut mgr = AutoConnectManager::new(1);
        let mut sp = MockSpawner::default();
        let recs = vec![
            backbone_rec("A", "10.0.0.5", 4965, 1),
            backbone_rec("B", "10.0.0.6", 4965, 2),
        ];
        mgr.poll(&recs, 1000.0, &mut sp);
        assert_eq!(sp.spawned.len(), 1, "cap of 1 permits only one autoconnect");
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn non_transport_records_are_skipped() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let mut rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);
        rec.transport = false;
        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        assert!(
            sp.spawned.is_empty(),
            "non-transport peer is not auto-connected"
        );
    }

    #[test]
    fn i2p_type_is_unimplemented_not_spawned() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        mgr.poll(&[i2p_rec(9)], 1000.0, &mut sp);
        assert!(sp.spawned.is_empty(), "I2P autoconnect is unimplemented");
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn failed_spawn_is_not_tracked_and_retried() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        sp.fail_next_spawn = true;
        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        assert_eq!(mgr.active_count(), 0, "a failed spawn is not tracked");

        // Next poll retries and succeeds.
        mgr.poll(std::slice::from_ref(&rec), 1001.0, &mut sp);
        assert_eq!(sp.spawned.len(), 1);
        assert_eq!(mgr.active_count(), 1);
    }

    /// #151: the manager hands the backing record to the spawner, so the
    /// spawner can resolve the interface's IFAC from the advertised
    /// netname/netkey (or refuse the endpoint, fail closed).
    #[test]
    fn spawner_receives_the_records_ifac_material() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let mut rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);
        rec.ifac_netname = Some("closednet".to_string());
        rec.ifac_netkey = Some("closedkey".to_string());

        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        assert_eq!(sp.spawned.len(), 1);
        assert_eq!(
            sp.spawned_ifac[0],
            (Some("closednet".to_string()), Some("closedkey".to_string())),
            "record IFAC material must reach the spawner"
        );
    }

    /// Drive the manager for `secs` simulated seconds at the production poll
    /// cadence (`driver::AUTOCONNECT_POLL_INTERVAL`, 1 s), starting at t=1000.
    /// Returns the slot occupancy sampled after every poll.
    fn simulate(
        mgr: &mut AutoConnectManager,
        live: &[DiscoveredInterfaceRecord],
        sp: &mut MockSpawner,
        secs: u64,
    ) -> Vec<usize> {
        (0..secs)
            .map(|t| {
                mgr.poll(live, 1000.0 + t as f64, sp);
                mgr.active_count()
            })
            .collect()
    }

    fn spawns_to(sp: &MockSpawner, host: &str) -> usize {
        sp.spawned.iter().filter(|s| s.1 == host).count()
    }

    /// #414, question 1, the case where usable candidates are scarcer than
    /// slots: three unroutable endpoints sort ahead of a single reachable peer
    /// under the issue's cap of three, so two slots keep cycling on dead
    /// endpoints for the whole run.
    ///
    /// The peer must still get its slot and keep it. It does, and already did
    /// before the cooldown: every dead endpoint detaches at the same tick, and
    /// the `torn_this_tick` guard makes the spawn pass skip them for that one
    /// poll, which is the peer's opening. This test pins that opening down, so
    /// a later change to the teardown pass cannot close it silently.
    #[test]
    fn unreachable_endpoints_do_not_starve_a_reachable_peer() {
        let mut mgr = AutoConnectManager::new(3);
        let mut sp = MockSpawner::default();
        for h in ["::", "2001:db8::1", "2001:db8::2"] {
            sp.never_online.insert(h.to_string());
        }
        // Caller-sorted best-first: the unroutable records rank above the peer.
        let live = vec![
            backbone_rec("dead-a", "::", 4242, 1),
            backbone_rec("dead-b", "2001:db8::1", 4242, 2),
            backbone_rec("dead-c", "2001:db8::2", 4242, 3),
            backbone_rec("good", "10.0.0.9", 4965, 4),
        ];

        let occupancy = simulate(&mut mgr, &live, &mut sp, 300);

        assert!(
            spawns_to(&sp, "10.0.0.9") >= 1,
            "reachable peer never got a slot in 5 min; occupancy={:?}, spawns={:?}",
            occupancy,
            sp.spawned.iter().map(|s| s.1.clone()).collect::<Vec<_>>(),
        );
        assert_eq!(
            spawns_to(&sp, "10.0.0.9"),
            1,
            "a peer that stays online is dialled exactly once"
        );
    }

    /// #414, question 1: do failing endpoints occupy auto-connect slots, so a
    /// host configured for three ends up with fewer usable ones?
    ///
    /// Measured answer: no, not durably. Three unroutable endpoints sort ahead
    /// of three reachable peers under the issue's cap of three. The unroutable
    /// ones hold every slot until the detach threshold; from then on the
    /// reachable peers hold all three and cannot be displaced, because an
    /// endpoint that is online is never torn down and the cap is full. The
    /// whole deficit is the one-off startup cycle, and the cooldown keeps it
    /// one-off rather than recurring.
    #[test]
    fn unreachable_endpoints_cost_one_startup_cycle_of_slot_time() {
        let mut mgr = AutoConnectManager::new(3);
        let mut sp = MockSpawner::default();
        for h in ["::", "2001:db8::1", "2001:db8::2"] {
            sp.never_online.insert(h.to_string());
        }
        let live = vec![
            backbone_rec("dead-a", "::", 4242, 1),
            backbone_rec("dead-b", "2001:db8::1", 4242, 2),
            backbone_rec("dead-c", "2001:db8::2", 4242, 3),
            backbone_rec("good-a", "10.0.0.9", 4965, 4),
            backbone_rec("good-b", "10.0.0.10", 4965, 5),
            backbone_rec("good-c", "10.0.0.11", 4965, 6),
        ];
        let reachable: Vec<[u8; 32]> = ["10.0.0.9", "10.0.0.10", "10.0.0.11"]
            .iter()
            .map(|h| AutoConnectManager::endpoint_hash(h, Some(4965)))
            .collect();

        let mut usable_per_sec = Vec::with_capacity(300);
        for t in 0..300u64 {
            mgr.poll(&live, 1000.0 + t as f64, &mut sp);
            usable_per_sec.push(
                mgr.active
                    .iter()
                    .filter(|a| reachable.contains(&a.endpoint_hash))
                    .count(),
            );
        }

        let empty_lead = usable_per_sec.iter().take_while(|n| **n == 0).count();
        assert!(
            empty_lead as f64 <= DETACH_THRESHOLD_SECS + 2.0,
            "reachable peers waited {empty_lead} s for a slot; one detach cycle is the budget"
        );
        assert!(
            usable_per_sec[empty_lead..].iter().all(|n| *n == 3),
            "all three slots must stay usable once handed over: {usable_per_sec:?}"
        );
    }

    /// #414, question 2: the retry cadence for an endpoint that has never once
    /// connected.
    ///
    /// Every teardown/respawn cycle destroys the TCP client and builds a fresh
    /// one, which resets that interface's own connect backoff to attempt 1 —
    /// so the interface layer's log throttling (`should_log_failure`: attempts
    /// 1..=3, then doublings) never gets past its base rate. Bounding the
    /// respawn rate is what bounds the log volume.
    #[test]
    fn a_never_connected_endpoint_is_retried_at_a_bounded_rate() {
        let mut mgr = AutoConnectManager::new(3);
        let mut sp = MockSpawner::default();
        sp.never_online.insert("::".to_string());
        let live = vec![backbone_rec("dead", "::", 4242, 1)];

        simulate(&mut mgr, &live, &mut sp, 300);

        let spawns = sp.spawned.len();
        assert!(
            spawns <= 6,
            "{spawns} dial cycles in 5 min for one unusable endpoint; each costs \
             three connect-failure warnings before the detach threshold cuts it"
        );
    }

    /// The cooldown is a backoff, not a blacklist: an endpoint that starts
    /// working is auto-connected again without operator action.
    #[test]
    fn a_cooled_down_endpoint_reconnects_once_it_comes_up() {
        let mut mgr = AutoConnectManager::new(1);
        let mut sp = MockSpawner::default();
        sp.never_online.insert("10.0.0.5".to_string());
        let live = vec![backbone_rec("peer", "10.0.0.5", 4965, 1)];

        simulate(&mut mgr, &live, &mut sp, 120);
        let cold_spawns = sp.spawned.len();
        assert!(cold_spawns >= 1, "the endpoint is tried at least once");

        // The peer comes up. Its next dial must stick.
        sp.never_online.remove("10.0.0.5");
        simulate(&mut mgr, &live, &mut sp, 600);

        assert!(
            sp.spawned.len() > cold_spawns,
            "a recovered endpoint must be dialled again, not blacklisted"
        );
        assert_eq!(mgr.active_count(), 1, "and must end up auto-connected");
    }

    /// A peer that connected once and later dropped is not a never-connected
    /// endpoint: it keeps Python's prompt re-attach after the detach threshold.
    #[test]
    fn a_peer_that_connected_once_is_reattached_promptly() {
        let mut mgr = AutoConnectManager::new(1);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("peer", "10.0.0.5", 4965, 1);
        let live = std::slice::from_ref(&rec);

        mgr.poll(live, 1000.0, &mut sp); // dialled
        let id = sp.spawned[0].3;
        mgr.poll(live, 1001.0, &mut sp); // observed online
        sp.offline.insert(id.0); // carrier dies
        for t in 0..20u64 {
            mgr.poll(live, 1002.0 + t as f64, &mut sp);
        }

        assert_eq!(sp.torn_down, vec![id], "sustained offline still detaches");
        assert_eq!(
            sp.spawned.len(),
            2,
            "a peer that has been online is re-dialled on the next poll"
        );
    }

    #[test]
    fn removed_interface_can_reconnect_on_rediscovery() {
        let mut mgr = AutoConnectManager::new(4);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        let id = sp.spawned[0].3;

        // Event loop saw a hard Disconnected and removed it out-of-band.
        mgr.on_interface_removed(id);
        assert_eq!(mgr.active_count(), 0);

        // Still discovered -> a later poll re-auto-connects.
        mgr.poll(std::slice::from_ref(&rec), 1001.0, &mut sp);
        assert_eq!(sp.spawned.len(), 2, "rediscovery re-auto-connects");
        assert_eq!(mgr.active_count(), 1);
    }
    /// Codeberg #416. The seed connection is only redundant once the
    /// auto-connect target is actually *online*: a spawned interface that has
    /// not connected, or one that is on its way out, is not connectivity.
    #[test]
    fn a_bootstrap_interface_is_kept_until_the_autoconnect_target_is_online() {
        let mut mgr = AutoConnectManager::new(1);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        // Nothing discovered yet: one seed is attached and stays.
        mgr.poll(&[], 1000.0, &mut sp);
        assert_eq!(mgr.bootstrap_action(1), BootstrapAction::Keep);

        // Dialled but not yet online -> still not a replacement.
        sp.offline.insert(0);
        mgr.poll(std::slice::from_ref(&rec), 1001.0, &mut sp);
        assert_eq!(
            mgr.bootstrap_action(1),
            BootstrapAction::Keep,
            "an auto-connected interface that never came online is not connectivity"
        );

        // Online, and the target is one: the seed has done its job.
        sp.offline.clear();
        mgr.poll(std::slice::from_ref(&rec), 1002.0, &mut sp);
        assert_eq!(mgr.bootstrap_action(1), BootstrapAction::Detach);
    }

    /// Codeberg #416, the other half: detaching the seed is only safe because
    /// it comes back. Losing the last auto-connected interface must ask for a
    /// re-attach, or the node is left with no way into the network at all.
    #[test]
    fn losing_every_autoconnect_asks_for_the_bootstrap_interface_back() {
        let mut mgr = AutoConnectManager::new(1);
        let mut sp = MockSpawner::default();
        let rec = backbone_rec("Hub", "10.0.0.5", 4965, 1);

        // The count is read before the spawn pass, as Python reads it at the
        // top of its monitor job: an endpoint dialled on this tick has not
        // connected yet, so it takes the following poll to count.
        mgr.poll(std::slice::from_ref(&rec), 1000.0, &mut sp);
        assert_eq!(mgr.bootstrap_action(1), BootstrapAction::Keep);
        mgr.poll(std::slice::from_ref(&rec), 1001.0, &mut sp);
        assert_eq!(mgr.bootstrap_action(1), BootstrapAction::Detach);

        // Seed detached; the record then disappears and the auto-connect with it.
        mgr.poll(&[], 1002.0, &mut sp);
        assert_eq!(mgr.active_count(), 0);
        assert_eq!(mgr.bootstrap_action(0), BootstrapAction::ReAttach);

        // Once it is back, nothing more is owed until the target is online again.
        assert_eq!(mgr.bootstrap_action(1), BootstrapAction::Keep);
    }

    /// With auto-connect off nothing could ever replace a seed connection, so
    /// the key stays inert rather than tearing down the node's only link.
    /// Python reaches the same place by another route: its monitor job only
    /// exists for auto-connected interfaces.
    #[test]
    fn bootstrap_only_is_inert_without_autoconnect() {
        let mut mgr = AutoConnectManager::new(0);
        let mut sp = MockSpawner::default();
        mgr.poll(&[backbone_rec("Hub", "10.0.0.5", 4965, 1)], 1000.0, &mut sp);
        assert_eq!(mgr.bootstrap_action(1), BootstrapAction::Keep);
        assert_eq!(
            mgr.bootstrap_action(0),
            BootstrapAction::Keep,
            "a disabled manager must not ask for interfaces it will never replace"
        );
    }

    /// A cap above one is reached only when every slot is online, so a partly
    /// filled auto-connect set keeps the seed.
    #[test]
    fn a_partly_filled_autoconnect_set_keeps_the_seed() {
        let mut mgr = AutoConnectManager::new(3);
        let mut sp = MockSpawner::default();
        let a = backbone_rec("A", "10.0.0.5", 4965, 1);
        let b = backbone_rec("B", "10.0.0.6", 4965, 2);

        mgr.poll(&[a.clone(), b.clone()], 1000.0, &mut sp);
        assert_eq!(mgr.active_count(), 2);
        assert_eq!(
            mgr.bootstrap_action(1),
            BootstrapAction::Keep,
            "two of three online is not the target"
        );

        let c = backbone_rec("C", "10.0.0.7", 4965, 3);
        mgr.poll(&[a.clone(), b.clone(), c.clone()], 1001.0, &mut sp);
        mgr.poll(&[a, b, c], 1002.0, &mut sp);
        assert_eq!(mgr.active_count(), 3);
        assert_eq!(mgr.bootstrap_action(1), BootstrapAction::Detach);
    }
}
