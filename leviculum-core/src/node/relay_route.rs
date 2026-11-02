//! Steering a link through a chosen relay (+ciris, leviculum#91, #92).
//!
//! The path table picks one next hop per destination. A host that wants to
//! choose (CIRIS: a peer behind a hard NAT relays through a contact before
//! the canonical node) reads [`NodeCore::relay_routes`], asks one relay with
//! [`NodeCore::request_path_on`], and opens links with
//! [`NodeCore::connect_via`]. Each such link keeps its relay for every
//! request attempt; two links to one destination through different relays
//! can be up at once (make-before-break, a warm standby). The learned path
//! table is not touched: links opened with `connect` route as before.

use alloc::vec::Vec;
use core::num::NonZeroU32;

use rand_core::CryptoRngCore;

use crate::constants::TRUNCATED_HASHBYTES;
use crate::destination::DestinationHash;
use crate::hex_fmt::HexShort;
use crate::link::{LinkError, LinkId};
use crate::relay_routes::{LinkVia, RelayRoute};
use crate::traits::{Clock, Storage};

use super::NodeCore;

/// Why [`NodeCore::connect_via`] opened no link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectViaError {
    /// The destination has not been heard through that relay (or the row
    /// expired, or its interface went away): there is nothing to route by.
    /// [`NodeCore::request_path_on`] the relay's interface first.
    NoRouteViaRelay,
    /// The link itself could not be created.
    Link(LinkError),
}

impl core::fmt::Display for ConnectViaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ConnectViaError::NoRouteViaRelay => {
                f.write_str("the destination was not heard through that relay")
            }
            ConnectViaError::Link(e) => write!(f, "{e}"),
        }
    }
}

impl<R: CryptoRngCore, C: Clock, S: Storage> NodeCore<R, C, S> {
    /// The relays `dest` has been heard through, freshest first: what
    /// [`connect_via`](Self::connect_via) can route by.
    pub fn relay_routes(&self, dest: &DestinationHash) -> Vec<RelayRoute> {
        let now_ms = self.transport.clock().now_ms();
        self.transport
            .relay_routes()
            .routes(dest.as_bytes(), now_ms)
    }

    /// Size the relay table: relays kept per destination and destinations
    /// kept. Off (0, 0) in core, so a board spends no heap on it; the std
    /// node turns it on at 4 per destination and 1024 destinations
    /// ([`crate::relay_routes::DEFAULT_RELAYS_PER_DESTINATION`],
    /// [`crate::relay_routes::DEFAULT_RELAY_DESTINATIONS`]). Either one 0
    /// turns it off.
    pub fn set_relay_route_bounds(&mut self, per_destination: usize, destinations: usize) {
        self.transport
            .relay_routes_mut()
            .set_bounds(per_destination, destinations);
    }

    /// Ask for a path to `dest` on one interface only (leviculum#92), so one
    /// relay behind it can answer without the request flooding every
    /// interface. An answer through a relay arrives as
    /// [`NodeEvent::RelayRouteLearned`](super::NodeEvent::RelayRouteLearned).
    /// Not throttled: the caller paces its own repeats.
    pub fn request_path_on(
        &mut self,
        dest: &DestinationHash,
        interface_index: usize,
    ) -> crate::transport::TickOutput {
        let mut tag = [0u8; TRUNCATED_HASHBYTES];
        self.rng.fill_bytes(&mut tag);
        if let Err(e) =
            self.transport
                .request_path_unthrottled(dest.as_bytes(), Some(interface_index), &tag)
        {
            crate::tracing::debug!(
                "Failed to build path request for <{}> on interface {}: {}",
                HexShort(dest.as_bytes()),
                interface_index,
                e
            );
        }
        self.process_events_and_actions()
    }

    /// Open a link to `dest` through `relay` (leviculum#91): the request
    /// names the relay as its next hop and goes out on the interface the
    /// relay was heard on, on every attempt. The link otherwise behaves as
    /// one from [`connect`](Self::connect).
    ///
    /// `establish_timeout_ms` is the host's budget for this relay: one
    /// attempt that long, no retry, so a relay that silently drops the
    /// request (it lost its own path) fails within the budget and the host
    /// moves to the next. `None` keeps the hop-scaled timeout and retries,
    /// all through the same relay.
    ///
    /// # Errors
    /// [`ConnectViaError::NoRouteViaRelay`] when `dest` has no live row for
    /// `relay`; [`ConnectViaError::Link`] as [`connect`](Self::connect).
    pub fn connect_via(
        &mut self,
        dest: DestinationHash,
        dest_signing_key: &[u8; 32],
        relay: [u8; TRUNCATED_HASHBYTES],
        establish_timeout_ms: Option<u32>,
    ) -> Result<(LinkId, crate::transport::TickOutput), ConnectViaError> {
        let now_ms = self.transport.clock().now_ms();
        let route = self
            .transport
            .relay_routes()
            .route(dest.as_bytes(), &relay, now_ms)
            .ok_or(ConnectViaError::NoRouteViaRelay)?;
        let via = LinkVia {
            relay,
            interface_index: route.interface_index,
            hops: route.hops,
            establish_timeout_ms: establish_timeout_ms.and_then(NonZeroU32::new),
        };
        crate::tracing::info!(
            dest = %HexShort(dest.as_bytes()),
            relay = %HexShort(&relay),
            interface = route.interface_index,
            hops = route.hops,
            "link via a chosen relay"
        );
        let (link_id, _routed, out) = self
            .connect_routed(dest, dest_signing_key, None, Some(via))
            .map_err(ConnectViaError::Link)?;
        Ok((link_id, out))
    }

    /// The relay a link goes through, when known (leviculum#91). Exact for
    /// a link opened with [`connect_via`](Self::connect_via). For any other
    /// link, the one relay heard on the link's interface, if exactly one
    /// was: the last relay strips its transport id from what it forwards,
    /// so a responder cannot read it off the wire.
    pub fn link_via_relay(&self, link_id: &LinkId) -> Option<[u8; TRUNCATED_HASHBYTES]> {
        let link = self.links.get(&self.resolve_link_id(link_id))?;
        if let Some(via) = link.via() {
            return Some(via.relay);
        }
        let iface = link.attached_interface()?;
        match self.transport.relay_routes().relays_on_interface(iface)[..] {
            [only] => Some(only),
            _ => None,
        }
    }
}
