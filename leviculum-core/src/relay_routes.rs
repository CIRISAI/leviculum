//! Every relay a destination was heard through (+ciris, leviculum#91).
//!
//! The path table keeps ONE next hop per destination, picked by hops and
//! announce age, and a copy of the same announce arriving through a second
//! transport node is discarded as a replay. A host that wants to choose its
//! relay (CIRIS: peers behind a hard NAT relay through contacts before the
//! canonical node) needs the alternatives too. This table records, beside
//! the path table and without changing it, each (destination, relay) pair a
//! validated announce or path response arrived through: the relay's
//! transport id, the interface it was heard on, the hop count and when.
//!
//! It is off unless a host sizes it ([`RelayRoutes::set_bounds`]; the std
//! node turns it on at [`DEFAULT_RELAYS_PER_DESTINATION`] by
//! [`DEFAULT_RELAY_DESTINATIONS`]): a board has no use for it and no heap
//! to spare. It is bounded per destination and in destinations. A row a live link
//! routed through it is using is pinned and never evicted; otherwise
//! eviction keeps interfaces distinct before it keeps rows fresh, so one
//! busy relay (heard first and most often) cannot crowd a second one out.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::constants::TRUNCATED_HASHBYTES;

/// Relays kept per destination, by default.
pub const DEFAULT_RELAYS_PER_DESTINATION: usize = 4;
/// Destinations with relay rows, at most, by default.
pub const DEFAULT_RELAY_DESTINATIONS: usize = 1024;

type Hash = [u8; TRUNCATED_HASHBYTES];

/// One way to reach a destination: through `relay`, heard on
/// `interface_index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayRoute {
    /// The relay's transport id (the next hop a link request names).
    pub relay: Hash,
    /// The interface the relay was heard on, which a request through it
    /// goes out on.
    pub interface_index: usize,
    /// Hops to the destination through this relay, as the announce counted.
    pub hops: u8,
    /// When an announce or path response last arrived through this relay.
    pub heard_at_ms: u64,
    /// When the row stops counting, unless heard again.
    pub expires_ms: u64,
}

/// How a link opened with `NodeCore::connect_via` is routed: through
/// `relay`, out of `interface_index`, for every request attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkVia {
    /// The relay's transport id.
    pub relay: Hash,
    /// The interface the relay was heard on.
    pub interface_index: usize,
    /// Hops to the destination through the relay.
    pub hops: u8,
    /// The host's establishment budget: one attempt, this long, no retry.
    pub establish_timeout_ms: Option<core::num::NonZeroU32>,
}

/// What recording a row changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorded {
    /// A relay not on file for the destination.
    New,
    /// A known relay, now on another interface or at another hop count.
    Changed,
    /// A known relay, unchanged but for its time.
    Refreshed,
    /// Not kept: the table is full of rows it prefers or must keep.
    Dropped,
}

/// The table.
#[derive(Debug)]
pub struct RelayRoutes {
    rows: BTreeMap<Hash, Vec<RelayRoute>>,
    /// (destination, relay) pairs a live link is using, with how many.
    pinned: BTreeMap<(Hash, Hash), u32>,
    per_destination: usize,
    max_destinations: usize,
}

impl Default for RelayRoutes {
    /// Off: records nothing until sized.
    fn default() -> Self {
        Self::new(0, 0)
    }
}

impl RelayRoutes {
    /// A table keeping `per_destination` relays for each of at most
    /// `max_destinations` destinations. Either one 0 turns it off.
    #[must_use]
    pub fn new(per_destination: usize, max_destinations: usize) -> Self {
        RelayRoutes {
            rows: BTreeMap::new(),
            pinned: BTreeMap::new(),
            per_destination,
            max_destinations,
        }
    }

    /// Change the bounds (either one 0 turns the table off and empties it);
    /// rows over a new, smaller bound go as they are next recorded over.
    pub fn set_bounds(&mut self, per_destination: usize, max_destinations: usize) {
        self.per_destination = per_destination;
        self.max_destinations = max_destinations;
        if !self.is_on() {
            self.rows.clear();
        }
    }

    /// Whether the table records anything.
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.per_destination > 0 && self.max_destinations > 0
    }

    fn is_pinned(&self, dest: &Hash, relay: &Hash) -> bool {
        self.pinned.contains_key(&(*dest, *relay))
    }

    /// Record that `dest` was heard through `route.relay`.
    pub fn record(&mut self, dest: Hash, route: RelayRoute) -> Recorded {
        if !self.is_on() {
            return Recorded::Dropped;
        }
        if let Some(rows) = self.rows.get_mut(&dest) {
            if let Some(row) = rows.iter_mut().find(|r| r.relay == route.relay) {
                let changed =
                    row.interface_index != route.interface_index || row.hops != route.hops;
                *row = route;
                return if changed {
                    Recorded::Changed
                } else {
                    Recorded::Refreshed
                };
            }
        } else if self.rows.len() >= self.max_destinations && !self.evict_destination(&dest) {
            return Recorded::Dropped;
        }
        let per_destination = self.per_destination;
        let pinned: Vec<Hash> = self
            .pinned
            .keys()
            .filter(|(d, _)| *d == dest)
            .map(|(_, r)| *r)
            .collect();
        let rows = self.rows.entry(dest).or_default();
        if rows.len() < per_destination {
            rows.push(route);
            return Recorded::New;
        }
        // Full: the candidate to give up is the one on the most crowded
        // interface (the new row counted), oldest first; the new row is the
        // freshest, so it only loses a tie on crowding to older rows.
        let crowd = |iface: usize, rows: &[RelayRoute]| {
            rows.iter().filter(|r| r.interface_index == iface).count()
                + usize::from(iface == route.interface_index)
        };
        let victim = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| !pinned.contains(&r.relay))
            .max_by(|(_, a), (_, b)| {
                crowd(a.interface_index, rows)
                    .cmp(&crowd(b.interface_index, rows))
                    .then(b.heard_at_ms.cmp(&a.heard_at_ms))
            })
            .map(|(i, r)| (i, crowd(r.interface_index, rows)));
        match victim {
            Some((i, victim_crowd)) if victim_crowd >= crowd(route.interface_index, rows) => {
                rows[i] = route;
                Recorded::New
            }
            _ => Recorded::Dropped,
        }
    }

    /// Make room for a new destination: drop the one heard from least
    /// recently that no link is using. False when every one is in use.
    fn evict_destination(&mut self, incoming: &Hash) -> bool {
        let victim = self
            .rows
            .iter()
            .filter(|(d, _)| *d != incoming && !self.pinned.keys().any(|(pd, _)| pd == *d))
            .min_by_key(|(_, rows)| rows.iter().map(|r| r.heard_at_ms).max().unwrap_or(0))
            .map(|(d, _)| *d);
        match victim {
            Some(d) => {
                self.rows.remove(&d);
                true
            }
            None => false,
        }
    }

    /// The live routes to `dest`, freshest first.
    #[must_use]
    pub fn routes(&self, dest: &Hash, now_ms: u64) -> Vec<RelayRoute> {
        let mut out: Vec<RelayRoute> = self
            .rows
            .get(dest)
            .map(|rows| {
                rows.iter()
                    .filter(|r| r.expires_ms > now_ms || self.is_pinned(dest, &r.relay))
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|r| core::cmp::Reverse(r.heard_at_ms));
        out
    }

    /// The live route to `dest` through `relay`.
    #[must_use]
    pub fn route(&self, dest: &Hash, relay: &Hash, now_ms: u64) -> Option<RelayRoute> {
        self.routes(dest, now_ms)
            .into_iter()
            .find(|r| &r.relay == relay)
    }

    /// The relays heard on `interface_index`, for any destination.
    #[must_use]
    pub fn relays_on_interface(&self, interface_index: usize) -> Vec<Hash> {
        let mut out: Vec<Hash> = Vec::new();
        for r in self.rows.values().flatten() {
            if r.interface_index == interface_index && !out.contains(&r.relay) {
                out.push(r.relay);
            }
        }
        out
    }

    /// A link now routes through `relay` to `dest`: keep the row.
    pub fn pin(&mut self, dest: Hash, relay: Hash) {
        *self.pinned.entry((dest, relay)).or_insert(0) += 1;
    }

    /// A link that routed through `relay` to `dest` is gone.
    pub fn unpin(&mut self, dest: &Hash, relay: &Hash) {
        let key = (*dest, *relay);
        if let Some(n) = self.pinned.get_mut(&key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.pinned.remove(&key);
            }
        }
    }

    /// Drop expired rows nothing pins.
    pub fn expire(&mut self, now_ms: u64) {
        let pinned = &self.pinned;
        self.rows.retain(|dest, rows| {
            rows.retain(|r| r.expires_ms > now_ms || pinned.contains_key(&(*dest, r.relay)));
            !rows.is_empty()
        });
    }

    /// An interface is gone: so is every route heard on it.
    pub fn interface_down(&mut self, interface_index: usize) {
        self.rows.retain(|_, rows| {
            rows.retain(|r| r.interface_index != interface_index);
            !rows.is_empty()
        });
    }

    /// Rows held, for the heap census.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        use crate::heap_census as hc;
        hc::btree_map_bytes(&self.rows)
            + self
                .rows
                .values()
                .map(|v| v.capacity() * core::mem::size_of::<RelayRoute>())
                .sum::<usize>()
            + hc::btree_map_bytes(&self.pinned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(relay: u8, iface: usize, heard: u64) -> RelayRoute {
        RelayRoute {
            relay: [relay; 16],
            interface_index: iface,
            hops: 2,
            heard_at_ms: heard,
            expires_ms: heard + 1_000_000,
        }
    }

    const D: Hash = [7; 16];

    #[test]
    fn a_second_relay_is_kept_beside_the_first_and_refreshes_in_place() {
        let mut t = RelayRoutes::default();
        assert_eq!(
            t.record(D, route(1, 0, 10)),
            Recorded::Dropped,
            "off by default"
        );
        t.set_bounds(DEFAULT_RELAYS_PER_DESTINATION, DEFAULT_RELAY_DESTINATIONS);
        assert_eq!(t.record(D, route(1, 0, 10)), Recorded::New);
        assert_eq!(t.record(D, route(2, 1, 20)), Recorded::New);
        assert_eq!(t.record(D, route(1, 0, 30)), Recorded::Refreshed);
        assert_eq!(t.record(D, route(1, 2, 40)), Recorded::Changed);
        let relays: Vec<_> = t.routes(&D, 50).iter().map(|r| r.relay[0]).collect();
        assert_eq!(relays, [1, 2], "freshest first");
    }

    #[test]
    fn a_full_destination_keeps_interfaces_distinct_before_fresh() {
        let mut t = RelayRoutes::new(2, 8);
        // The busy relay on interface 0, then a contact on interface 1.
        t.record(D, route(1, 0, 10));
        t.record(D, route(2, 1, 20));
        // A newer relay on interface 0 again replaces the older one there,
        // not the contact.
        assert_eq!(t.record(D, route(3, 0, 30)), Recorded::New);
        let mut relays: Vec<_> = t.routes(&D, 40).iter().map(|r| r.relay[0]).collect();
        relays.sort_unstable();
        assert_eq!(relays, [2, 3]);
    }

    #[test]
    fn a_pinned_row_is_never_evicted_or_expired() {
        let mut t = RelayRoutes::new(1, 8);
        t.record(D, route(1, 0, 10));
        t.pin(D, [1; 16]);
        assert_eq!(t.record(D, route(2, 0, 20)), Recorded::Dropped);
        t.expire(u64::MAX);
        assert_eq!(t.routes(&D, u64::MAX).len(), 1);
        t.unpin(&D, &[1; 16]);
        t.expire(u64::MAX);
        assert!(t.routes(&D, 0).is_empty());
    }

    #[test]
    fn destinations_are_bounded_and_a_lost_interface_takes_its_routes() {
        let mut t = RelayRoutes::new(4, 2);
        t.record([1; 16], route(1, 0, 10));
        t.record([2; 16], route(1, 1, 20));
        assert_eq!(t.record([3; 16], route(1, 1, 30)), Recorded::New);
        assert!(t.routes(&[1; 16], 40).is_empty(), "the stalest went");
        t.interface_down(1);
        assert!(t.routes(&[2; 16], 40).is_empty());
        assert!(t.routes(&[3; 16], 40).is_empty());
    }
}
